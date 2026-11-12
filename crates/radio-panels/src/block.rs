use crate::{clean, match_key, text::py_strip};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;
/// The answer format used by the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    List,
    Text,
}
impl Kind {
    /// Parse a protocol keyword.
    pub fn parse(word: &str) -> Option<Kind> {
        match word {
            "list" => Some(Self::List),
            "text" => Some(Self::Text),
            _ => None,
        }
    }
}

/// A member's declared stance in a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Propose,
    Agree,
    Object,
}

/// A validated, cleaned member post.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub position: Position,
    pub items: Vec<String>,
    pub draft: Option<String>,
    pub draft_id: Option<String>,
    pub agree_with: Option<String>,
    pub ratings: BTreeMap<String, i64>,
    pub objections: String,
}

/// Read and validate only the last complete deliberation fence.
pub fn parse_block(
    post: &str,
    kind: Kind,
    n: Option<i64>,
    round_no: u32,
    author: &str,
    crew: &[String],
    known_drafts: &BTreeSet<String>,
) -> Option<Block> {
    let body = last_body(post)?;
    let fields = fields(body)?;
    let position = match fields.get("position")?.as_str()? {
        "propose" => Position::Propose,
        "agree" => Position::Agree,
        "object" => Position::Object,
        _ => return None,
    };
    let objections = fields
        .get("objections")
        .map_or_else(|| Some(String::new()), |v| bounded(v, 600))?;
    let mut block = Block {
        position,
        items: Vec::new(),
        draft: None,
        draft_id: None,
        agree_with: None,
        ratings: BTreeMap::new(),
        objections,
    };
    if kind == Kind::List {
        let n = n.filter(|n| (1..=12).contains(n))? as usize;
        if ["draft", "agree_with", "ratings"]
            .iter()
            .any(|k| fields.contains_key(*k))
        {
            return None;
        }
        let raw = fields.get("items")?.as_array()?;
        if raw.is_empty() || raw.len() > n {
            return None;
        }
        let mut seen = BTreeSet::new();
        for value in raw {
            let item = bounded(value, 120)?;
            if item.is_empty() {
                return None;
            }
            if seen.insert(match_key(&item)) {
                block.items.push(item);
            }
        }
        return Some(block);
    }
    if fields.contains_key("items") {
        return None;
    }
    if let Some(value) = fields.get("draft") {
        block.draft = Some(bounded(value, 1500)?);
        block.draft_id = Some(format!("{author}.r{round_no}"));
    } else if position != Position::Agree {
        return None;
    }
    if position == Position::Agree {
        let id = fields.get("agree_with")?.as_str()?;
        if !known_drafts.contains(id) {
            return None;
        }
        block.agree_with = Some(id.to_owned());
    } else if fields.contains_key("agree_with") {
        return None;
    }
    if let Some(value) = fields.get("ratings") {
        for (name, value) in value.as_object()? {
            let rating = value.as_i64()?;
            if name == author || !crew.contains(name) || !(0..=10).contains(&rating) {
                return None;
            }
            block.ratings.insert(name.clone(), rating);
        }
    }
    Some(block)
}

fn bounded(value: &Value, limit: usize) -> Option<String> {
    let cleaned = clean(value.as_str()?, 10_000);
    (cleaned.chars().count() <= limit).then_some(cleaned)
}

fn last_body(post: &str) -> Option<&str> {
    let mut start = None;
    let mut length = 0;
    let mut offset: usize = 0;
    let mut last = None;
    // Match fences without changing body CR characters or splitting on other separators.
    for raw in post.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let ticks = line.bytes().take_while(|b| *b == b'`').count();
        let rest = line.get(ticks..)?;
        if let Some(body_start) = start {
            if ticks >= length && rest.bytes().all(|b| b == b' ') {
                last = post.get(body_start..offset.saturating_sub(1));
                start = None;
            }
        } else if ticks >= 3
            && rest
                .strip_prefix("deliberation")
                .is_some_and(|s| s.bytes().all(|b| b == b' '))
        {
            start = Some(offset + raw.len() + 1);
            length = ticks;
        }
        offset += raw.len() + 1;
    }
    last
}

static JSON_NUMBER: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"\A-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?\z"));

fn fields(body: &str) -> Option<BTreeMap<String, Value>> {
    let mut fields = BTreeMap::new();
    for line in body.split('\n').filter(|s| !py_strip(s).is_empty()) {
        let (key, value) = line.split_once(':')?;
        let (key, value) = (py_strip(key), py_strip(value));
        if ![
            "position",
            "items",
            "draft",
            "agree_with",
            "ratings",
            "objections",
        ]
        .contains(&key)
            || fields.contains_key(key)
        {
            return None;
        }
        let parsed = if value.starts_with(['"', '[', '{']) {
            let normalized = normalize_json(value);
            let mut stream = serde_json::Deserializer::from_str(&normalized).into_iter::<Value>();
            let parsed = stream.next()?.ok()?;
            let rest = py_strip(normalized.get(stream.byte_offset()..)?);
            if !rest.is_empty() && !rest.starts_with('#') {
                return None;
            }
            parsed
        } else {
            let value = py_strip(value.split_once(" #").map_or(value, |(v, _)| v));
            if ["NaN", "Infinity", "-Infinity"].contains(&value) {
                Value::Null
            } else {
                serde_json::from_str(value).unwrap_or_else(|_| {
                    if JSON_NUMBER.as_ref().is_ok_and(|r| r.is_match(value)) {
                        Value::Null
                    } else {
                        Value::String(value.to_owned())
                    }
                })
            }
        };
        fields.insert(key.to_owned(), parsed);
    }
    Some(fields)
}

fn normalize_json(value: &str) -> String {
    let mut result = String::new();
    let mut chars = value.chars().peekable();
    let (mut inside, mut escaped) = (false, false);
    // Python's non-strict JSON accepts raw controls; preserve integer negative zero too.
    while let Some(c) = chars.next() {
        if inside {
            if c <= '\u{001f}' {
                // Leave invalid escapes for serde to reject; trailing comments are ignored.
                if escaped {
                    result.push(c);
                } else {
                    result.push_str(&format!("\\u{:04x}", c as u32));
                }
            } else {
                result.push(c);
            }
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                inside = false;
            }
        } else if c == '"' {
            inside = true;
            result.push(c);
        } else if c == '-' && chars.peek() == Some(&'0') {
            chars.next();
            if chars
                .peek()
                .is_some_and(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E'))
            {
                result.push('-');
            }
            result.push('0');
        } else {
            result.push(c);
        }
    }
    result
}
