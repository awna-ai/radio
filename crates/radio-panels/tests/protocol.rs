use radio_panels::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    fs,
    path::Path,
};

fn ok<T: Debug>(result: Result<T, ProtocolError>) -> T {
    assert!(result.is_ok(), "expected Ok, got {result:?}");
    result.unwrap()
}

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn clean_controls_invisibles_and_character_limit() {
    assert_eq!(
        clean(" \x00a\u{200b}b\u{2028}c\u{2069}d\u{feff}\n\tZ ", 100),
        "a b c d \n\tZ"
    );
    assert_eq!(clean("🙂abc", 2), "🙂a…");
    assert_eq!(clean("abc", 0), "…");
    assert_eq!(clean("a\n\tb", 4), "a\n\tb");
}

#[test]
fn match_key_is_exact_after_whitespace_and_casefold() {
    assert_eq!(match_key(" STRAẞE\n\t name "), "strasse name");
    assert_ne!(match_key("cat"), match_key("cats"));
}

#[test]
fn fence_outgrows_backticks_and_validates_label() {
    assert_eq!(
        ok(fence("a/b (v1)", "x```y")),
        "````text a/b (v1)\nx```y\n````"
    );
    for label in [String::new(), "x\n".into(), "x`".into(), "x".repeat(81)] {
        assert_eq!(
            fence(&label, "body"),
            Err(ProtocolError::InvalidFenceLabel),
            "{label:?}"
        );
    }
}

#[test]
fn home_relative_path_is_a_leak_on_its_own() {
    assert_eq!(leak_findings("Read ~/x/ please", &[]), ["path"]);
}

#[test]
fn leaks_return_only_sorted_categories() {
    assert_eq!(
        leak_findings(
            "a+b@example.org (/Users/a/) ~/x/ SECRET",
            &names(&["secret", ""])
        ),
        ["email", "path", "term"]
    );
    assert_eq!(
        leak_findings("a/b and/or secrets", &names(&["secret"])),
        Vec::<&str>::new()
    );
    assert_eq!(leak_findings("(a.b)", &names(&["a.b"])), ["term"]);
}

#[test]
fn leak_term_over_a_thousand_characters_fails_closed() {
    assert_eq!(leak_findings("plain words", &["q".repeat(1001)]), ["term"]);
    assert_eq!(
        leak_findings("plain words", &["q".repeat(1000)]),
        Vec::<&str>::new()
    );
}

#[test]
fn roundtrip_is_json_and_reopen_resets_only_stop_state() {
    let mut state = ok(DeliberationState::new(
        Kind::Text,
        None,
        names(&["a", "b", "c"]),
    ));
    state.last_block.insert(
        "a".into(),
        Some(Block {
            position: Position::Propose,
            items: vec![],
            draft: Some("plan".into()),
            draft_id: Some("a.r8".into()),
            agree_with: None,
            ratings: BTreeMap::from([("b".into(), 7)]),
            objections: String::new(),
        }),
    );
    state.drafts.insert("a.r8".into(), "plan".into());
    state.round_no = 8;
    state.prev_d = Some(0.4);
    state.prev_own = BTreeMap::from([("a".into(), 0.2), ("b".into(), 0.3), ("c".into(), 0.7)]);
    state.d_history = vec![1.0, 0.4];
    state.closed = Some(Outcome::Deadlock);
    state.no_progress = 4;
    state.holdout.insert("c".into(), true);
    state.stall.insert("c".into(), 2);
    state.weight.insert("c".into(), 0.125);
    let encoded: Value =
        serde_json::from_str(&serde_json::to_string(&state.to_json()).expect("JSON string"))
            .expect("JSON value");
    // Pin the wire names as well as round-trip equality; two broken serializers must not agree.
    assert_eq!(encoded["prev_D"], json!(0.4));
    assert_eq!(encoded["last_block"]["a"]["items"], json!([]));
    let mut copy = ok(DeliberationState::from_json(&encoded));
    assert_eq!(copy.to_json(), encoded);
    reopen(&mut copy);
    assert_eq!(copy.closed, None);
    assert_eq!(copy.no_progress, 0);
    assert_eq!(
        copy.weight,
        BTreeMap::from([("a".into(), 1.0), ("b".into(), 1.0), ("c".into(), 1.0)])
    );
    assert!(copy.holdout.values().all(|value| !value));
    assert!(copy.stall.values().all(|value| *value == 0));
    for key in [
        "round_no",
        "prev_D",
        "prev_own",
        "last_block",
        "d_history",
        "drafts",
    ] {
        assert_eq!(copy.to_json()[key], encoded[key], "{key}");
    }
    assert_eq!(state.weight["c"], 0.125);
}

#[test]
fn invalid_state_configuration() {
    for (kind, n, crew) in [
        (Kind::List, Some(0), names(&["a", "b"])),
        (Kind::List, Some(13), names(&["a", "b"])),
        (Kind::Text, None, names(&["a"])),
        (Kind::Text, None, names(&["a", "a"])),
    ] {
        assert!(matches!(
            DeliberationState::new(kind, n, crew),
            Err(ProtocolError::InvalidState(_))
        ));
    }
    // The typed constructor cannot receive the reference's "other" kind.
    assert_eq!(Kind::parse("other"), None);
    let mut value = state_json();
    value["kind"] = json!("other");
    assert!(matches!(
        DeliberationState::from_json(&value),
        Err(ProtocolError::InvalidState(_))
    ));
}

#[test]
fn closed_round_requires_reopen() {
    let mut state = ok(DeliberationState::new(
        Kind::List,
        Some(1),
        names(&["a", "b"]),
    ));
    state.closed = Some(Outcome::Deadlock);
    assert_eq!(
        step_round(&mut state, &BTreeMap::new()),
        Err(ProtocolError::Closed)
    );
    assert_eq!(state.round_no, 0);
}

#[test]
fn zero_weight_means_and_missing_blocks() {
    let matrix = BTreeMap::from([
        ("a".into(), BTreeMap::from([("b".into(), 0.25)])),
        ("b".into(), BTreeMap::from([("a".into(), 0.25)])),
    ]);
    let (own, group) = ok(distances(
        &names(&["a", "b"]),
        &matrix,
        &BTreeMap::from([("a".into(), 0.0), ("b".into(), 0.0)]),
    ));
    assert_eq!(
        own,
        BTreeMap::from([("a".into(), 0.25), ("b".into(), 0.25)])
    );
    assert_eq!(group, 0.25);
    assert_eq!(
        ok(pair_distance(None, None, Kind::List, Some(3), "a", "b")),
        1.0
    );
}

#[test]
fn zero_pair_weight_fallback_with_nonzero_member() {
    let matrix = BTreeMap::from([
        (
            "a".into(),
            BTreeMap::from([("b".into(), 0.2), ("c".into(), 0.4)]),
        ),
        (
            "b".into(),
            BTreeMap::from([("a".into(), 0.2), ("c".into(), 0.8)]),
        ),
        (
            "c".into(),
            BTreeMap::from([("a".into(), 0.4), ("b".into(), 0.8)]),
        ),
    ]);
    let (own, group) = ok(distances(
        &names(&["a", "b", "c"]),
        &matrix,
        &BTreeMap::from([("a".into(), 1.0), ("b".into(), 0.0), ("c".into(), 0.0)]),
    ));
    assert!((own.get("a").copied().unwrap_or(f64::NAN) - 0.3).abs() < 1e-7);
    assert_eq!(own.get("b"), Some(&0.2));
    assert_eq!(own.get("c"), Some(&0.4));
    assert!((group - 7.0 / 15.0).abs() < 1e-7);
}

#[test]
fn markdown_quotes_the_complete_fixture_objection() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dissent_quoted_in_full.json");
    let fixture: Value = serde_json::from_str(&fs::read_to_string(path).expect("fixture file"))
        .expect("fixture JSON");
    let actual = ok(evaluate_fixture(&fixture));
    let result: PanelResult =
        serde_json::from_value(actual["result"].clone()).expect("result shape");
    let expected = fixture["expect"]["result"]["dissent"][0]["objections"]
        .as_str()
        .expect("objections");
    let rendered = ok(render_markdown(&result));
    assert!(rendered.starts_with("# Deliberation result — proposal, not a decision\n"));
    assert!(rendered.contains("## Dissent\n"));
    assert!(rendered.contains(&ok(fence("objections from b", expected))));
    assert!(rendered.contains("weight: 0.50"));
    assert!(rendered.contains("## Trajectory\n"));
    assert_eq!(expected.chars().count(), 600);
}

#[test]
fn invalid_timebox() {
    assert_eq!(TimeBox::parse("5m"), Err(ProtocolError::UnknownTimeBox));
}

fn state_json() -> Value {
    json!({"kind":"text", "n":null, "crew":["a","b"], "round_no":0,
        "last_block":{"a":null,"b":null}, "weight":{"a":1.0,"b":1.0},
        "holdout":{"a":false,"b":false}, "stall":{"a":0,"b":0},
        "prev_own":{"a":1.0,"b":1.0}, "prev_D":null, "no_progress":0,
        "closed":null, "d_history":[], "drafts":{}})
}

#[test]
fn state_json_refuses_unknown_key() {
    let mut value = state_json();
    value["unknown"] = json!(true);
    assert!(matches!(
        DeliberationState::from_json(&value),
        Err(ProtocolError::InvalidState(_))
    ));
}

#[test]
fn state_json_refuses_twenty_five_members() {
    let mut value = state_json();
    value["crew"] = json!((0..25).map(|i| format!("m{i}")).collect::<Vec<_>>());
    assert!(matches!(
        DeliberationState::from_json(&value),
        Err(ProtocolError::InvalidState(_))
    ));
}

#[test]
fn state_json_refuses_repeated_member() {
    let mut value = state_json();
    value["crew"] = json!(["a", "a"]);
    assert!(matches!(
        DeliberationState::from_json(&value),
        Err(ProtocolError::InvalidState(_))
    ));
}

#[test]
fn state_json_fills_missing_weight() {
    let mut value = state_json();
    value["weight"]
        .as_object_mut()
        .expect("weight map")
        .remove("b");
    let state = ok(DeliberationState::from_json(&value));
    assert_eq!(state.weight.get("a"), Some(&1.0));
    assert_eq!(state.weight.get("b"), Some(&1.0));
}

fn parse_text(post: &str) -> Option<Block> {
    parse_block(
        post,
        Kind::Text,
        None,
        1,
        "a",
        &names(&["a", "b"]),
        &BTreeSet::new(),
    )
}

fn valid_post() -> &'static str {
    "```deliberation\nposition: propose\ndraft: plan\n```"
}

#[test]
fn hostile_hundred_thousand_backticks() {
    assert_eq!(parse_text(&"`".repeat(100_000)), None);
}

#[test]
fn hostile_fifty_thousand_fence_lines_last_body_valid() {
    // 24,999 empty pairs followed by one complete valid pair: 50,000 fence lines.
    let post = "```deliberation\n```\n".repeat(24_999) + valid_post();
    let block = parse_text(&post);
    assert!(block.is_some(), "last complete valid block must survive");
    assert_eq!(block.unwrap().draft.as_deref(), Some("plan"));
}

#[test]
fn hostile_ten_thousand_unclosed_openings_preserve_valid_block() {
    let post = format!("{}\n{}", valid_post(), "```deliberation\n".repeat(10_000));
    let block = parse_text(&post);
    assert!(
        block.is_some(),
        "unclosed openings must not replace the last complete block"
    );
    assert_eq!(block.unwrap().draft.as_deref(), Some("plan"));
}

#[test]
fn hostile_five_thousand_emoji_draft() {
    assert_eq!(
        parse_text(&format!(
            "```deliberation\nposition: propose\ndraft: {}\n```",
            "🙂".repeat(5_000)
        )),
        None
    );
}

#[test]
fn hostile_carriage_return_alone() {
    assert_eq!(parse_text("\r"), None);
}

#[test]
fn hostile_five_hundred_nested_arrays() {
    assert_eq!(
        parse_text(&format!(
            "```deliberation\nposition: propose\ndraft: {}\"x\"{}\n```",
            "[".repeat(500),
            "]".repeat(500)
        )),
        None
    );
}

#[test]
fn hostile_json_string_with_every_control_character() {
    let controls: String = ('\u{0000}'..='\u{001f}').collect();
    // Keep controls inside one field line; escaped newlines become raw text only after JSON parsing.
    let draft = serde_json::to_string(&format!("a{controls}z")).expect("control JSON");
    let block = parse_text(&format!(
        "```deliberation\nposition: propose\ndraft: {draft}\n```"
    ));
    assert!(
        block.is_some(),
        "JSON controls must parse and then be cleaned"
    );
    assert_eq!(
        block.unwrap().draft,
        Some(format!("a{}\t\n{}z", " ".repeat(9), " ".repeat(21)))
    );
}
