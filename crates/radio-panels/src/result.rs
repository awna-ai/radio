use crate::{
    Answer, DeliberationState, Kind, Outcome, Position, ProtocolError, RoundReport,
    distance_matrix, fence, match_key, sum::py_sum,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
/// A connected set of members and its proposed answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub members: Vec<String>,
    pub weight: f64,
    pub answer: Option<Answer>,
}
/// A member's objections retained in the final proposal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dissent {
    pub member: String,
    pub holdout: bool,
    pub objections: String,
}
/// The observed group distances and round count.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trajectory {
    #[serde(rename = "D")]
    pub d: Vec<f64>,
    pub rounds: u32,
}
/// A proposal with its agreement groups, dissent and trajectory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelResult {
    pub label: String,   // "Proposal for the principals, not a decision"
    pub outcome: String, // "agreed" | "deadlock" | "open"
    pub kind: String,    // "list:<n>" | "text"
    pub answer: Option<Answer>,
    pub groups: Vec<Group>,
    pub dissent: Vec<Dissent>,
    pub trajectory: Trajectory,
}
/// Find connected agreement groups, ranked by weight, size and member order.
pub fn groups(state: &DeliberationState, last: &RoundReport) -> Result<Vec<Group>, ProtocolError> {
    let matrix = distance_matrix(state.kind, state.n, &state.crew, &state.last_block)?;
    let mut remaining: BTreeSet<_> = state.crew.iter().cloned().collect();
    let mut groups = Vec::new();
    for first in &state.crew {
        if !remaining.remove(first) {
            continue;
        }
        let mut component = BTreeSet::from([first.clone()]);
        let mut pending = vec![first.clone()];
        while let Some(name) = pending.pop() {
            for other in &state.crew {
                if remaining.contains(other) && matrix[&name][other] <= 1.0 / 3.0 + 1e-12 {
                    remaining.remove(other);
                    component.insert(other.clone());
                    pending.push(other.clone());
                }
            }
        }
        let members: Vec<_> = state
            .crew
            .iter()
            .filter(|n| component.contains(*n))
            .cloned()
            .collect();
        let answer = if state.kind == Kind::List {
            Some(Answer::List(list_answer(state, &members)))
        } else {
            text_answer(state, &members)?.map(Answer::Text)
        };
        let weight = py_sum(
            members
                .iter()
                .map(|n| last.weights.get(n).copied().unwrap_or(1.0)),
        ) / state.crew.len() as f64;
        groups.push(Group {
            members,
            weight,
            answer,
        });
    }
    groups.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| b.members.len().cmp(&a.members.len()))
            .then_with(|| {
                let rank = |g: &Group| state.crew.iter().position(|n| g.members.first() == Some(n));
                rank(a).cmp(&rank(b))
            })
    });
    Ok(groups)
}

fn list_answer(state: &DeliberationState, members: &[String]) -> Vec<String> {
    let mut positions: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut display = BTreeMap::new();
    for name in members {
        if let Some(Some(block)) = state.last_block.get(name) {
            let mut seen = BTreeSet::new();
            for (position, item) in block.items.iter().enumerate() {
                let key = match_key(item);
                if seen.insert(key.clone()) {
                    positions.entry(key.clone()).or_default().push(position);
                    display.entry(key).or_insert_with(|| item.clone());
                }
            }
        }
    }
    let mut keys: Vec<_> = positions
        .keys()
        .filter(|k| positions[*k].len() * 2 > members.len())
        .collect();
    keys.sort_by(|a, b| {
        let pa = &positions[*a];
        let pb = &positions[*b];
        let mean = |p: &[usize]| p.iter().sum::<usize>() as f64 / p.len() as f64;
        pb.len()
            .cmp(&pa.len())
            .then_with(|| mean(pa).total_cmp(&mean(pb)))
            .then_with(|| a.cmp(b))
    });
    keys.into_iter().map(|k| display[k].clone()).collect()
}

fn text_answer(
    state: &DeliberationState,
    members: &[String],
) -> Result<Option<String>, ProtocolError> {
    let mut counts = BTreeMap::<String, usize>::new();
    for name in members {
        if let Some(Some(block)) = state.last_block.get(name) {
            let held = if block.position == Position::Agree {
                &block.agree_with
            } else {
                &block.draft_id
            };
            if let Some(id) = held {
                *counts.entry(id.clone()).or_default() += 1;
            }
        }
    }
    let mut candidates = Vec::new();
    for (id, count) in counts {
        let round = id
            .rsplit_once(".r")
            .and_then(|(_, r)| r.trim().parse::<i64>().ok())
            .ok_or(ProtocolError::BadDraftId)?;
        candidates.push((id, count, round));
    }
    candidates.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(candidates
        .first()
        .and_then(|(id, _, _)| state.drafts.get(id))
        .cloned())
}

/// Preserve objections from holdouts and members outside the leading group.
pub fn dissent(state: &DeliberationState, groups: &[Group]) -> Vec<Dissent> {
    state
        .crew
        .iter()
        .filter(|name| {
            state.holdout.get(*name).copied().unwrap_or(false)
                || !groups.first().is_some_and(|g| g.members.contains(name))
        })
        .map(|name| Dissent {
            member: name.clone(),
            holdout: state.holdout.get(name).copied().unwrap_or(false),
            objections: state
                .last_block
                .get(name)
                .and_then(Option::as_ref)
                .map(|b| b.objections.clone())
                .unwrap_or_default(),
        })
        .collect()
}
/// Build a proposal, resolving agreed draft ids through the draft archive.
pub fn build_result(
    state: &DeliberationState,
    last: &RoundReport,
) -> Result<PanelResult, ProtocolError> {
    let groups = groups(state, last)?;
    let answer = if state.closed == Some(Outcome::Agreed) {
        match &last.agreed_answer {
            Some(Answer::List(items)) => Some(Answer::List(items.clone())),
            Some(Answer::Text(id)) => state.drafts.get(id).cloned().map(Answer::Text),
            None => None,
        }
    } else {
        None
    };
    Ok(PanelResult {
        label: "Proposal for the principals, not a decision".to_owned(),
        outcome: match state.closed {
            Some(Outcome::Agreed) => "agreed",
            Some(Outcome::Deadlock) => "deadlock",
            None => "open",
        }
        .to_owned(),
        kind: if state.kind == Kind::List {
            format!("list:{}", state.n.ok_or(ProtocolError::ListNeedsN)?)
        } else {
            "text".to_owned()
        },
        answer,
        dissent: dissent(state, &groups),
        groups,
        trajectory: Trajectory {
            d: state.d_history.clone(),
            rounds: state.round_no,
        },
    })
}

fn answer_text(answer: &Option<Answer>) -> Result<String, ProtocolError> {
    match answer {
        None => Ok("None".to_owned()),
        Some(Answer::Text(s)) => Ok(s.clone()),
        Some(Answer::List(items)) => {
            let items: Result<Vec<_>, _> = items.iter().map(serde_json::to_string).collect();
            Ok(format!(
                "[{}]",
                items
                    .map_err(|_| ProtocolError::InvalidState("invalid answer"))?
                    .join(", ")
            ))
        }
    }
}

fn py_float_repr(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    // Python uses plain notation only for decimal exponents in -4..16.
    let scientific = format!("{:e}", x.abs());
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return scientific;
    };
    let e = exponent.parse::<i32>().unwrap_or(0);
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if !(-4..16).contains(&e) {
        return format!("{sign}{mantissa}e{e:+03}");
    }
    let digits = mantissa.replace('.', "");
    let decimal = e + 1;
    if decimal <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-decimal) as usize))
    } else if decimal as usize >= digits.len() {
        format!(
            "{sign}{digits}{}.0",
            "0".repeat(decimal as usize - digits.len())
        )
    } else {
        let (left, right) = digits.split_at(decimal as usize);
        format!("{sign}{left}.{right}")
    }
}

/// Render the reference markdown, fencing all answers and objections.
pub fn render_markdown(result: &PanelResult) -> Result<String, ProtocolError> {
    let mut lines = vec![
        "# Deliberation result — proposal, not a decision".to_owned(),
        String::new(),
        format!("Outcome: {}", result.outcome),
        String::new(),
        "## Answer".to_owned(),
        String::new(),
        fence("answer", &answer_text(&result.answer)?)?,
        String::new(),
        "## Groups".to_owned(),
        String::new(),
    ];
    for group in &result.groups {
        lines.extend([
            format!(
                "Members: {}; weight: {:.2}",
                group.members.join(", "),
                group.weight
            ),
            String::new(),
            fence("group answer", &answer_text(&group.answer)?)?,
            String::new(),
        ]);
    }
    if !result.dissent.is_empty() {
        lines.extend(["## Dissent".to_owned(), String::new()]);
        for entry in &result.dissent {
            lines.extend([
                fence(
                    &format!("objections from {}", entry.member),
                    &entry.objections,
                )?,
                String::new(),
            ]);
        }
    }
    lines.extend([
        "## Trajectory".to_owned(),
        String::new(),
        format!("Rounds: {}", result.trajectory.rounds),
        format!(
            "D: [{}]",
            result
                .trajectory
                .d
                .iter()
                .map(|d| py_float_repr(*d))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ]);
    Ok(lines.join("\n") + "\n")
}
