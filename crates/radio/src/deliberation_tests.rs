use super::*;
use serde_json::Value;
use std::{fs, path::PathBuf};

fn seat(name: &str) -> Author {
    Author::member("helper", name)
}

fn seats(names: &[&str]) -> Vec<Author> {
    names.iter().map(|name| seat(name)).collect()
}

fn fixtures() -> Vec<Value> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../radio-panels/tests/fixtures");
    let mut paths: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
        .collect()
}

/// Writes a fixture's block fields as the fenced block a seat would post.
fn post(fields: &Value) -> String {
    let mut lines = vec!["Thinking it over.".to_owned(), "```deliberation".to_owned()];
    for key in [
        "position",
        "items",
        "draft",
        "agree_with",
        "ratings",
        "objections",
    ] {
        match fields.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) if key == "position" => lines.push(format!("{key}: {text}")),
            Some(Value::String(text)) if key == "objections" && text.is_empty() => {}
            Some(Value::Object(map)) if map.is_empty() => {}
            Some(value) => lines.push(format!("{key}: {value}")),
        }
    }
    lines.push("```".to_owned());
    lines.join("\n")
}

#[test]
fn every_full_round_fixture_steps_to_the_reference_result() {
    let mut replayed = 0;
    for fixture in fixtures() {
        let input = &fixture["input"];
        if fixture["type"] != "rounds" || !input["reopen_after"].is_null() {
            continue;
        }
        let crew: Vec<&str> = input["crew"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        let rounds = input["rounds"].as_array().unwrap();
        let full = rounds.iter().all(|round| {
            crew.iter()
                .all(|name| round.get(*name).is_some_and(|block| !block.is_null()))
        });
        if !full {
            continue;
        }
        let name = fixture["name"].as_str().unwrap();
        let mut record = Record::convene(
            input["kind"].as_str().unwrap(),
            input["n"].as_i64(),
            seats(&crew),
        )
        .unwrap();
        for (index, round) in rounds.iter().enumerate() {
            for (at, member) in crew.iter().enumerate() {
                assert_eq!(record.view().round, index as u32, "{name}");
                let stepped = record
                    .turn(&seat(member), &post(&round[*member]))
                    .unwrap_or_else(|error| panic!("{name} round {index} {member}: {error}"));
                assert_eq!(stepped, at + 1 == crew.len(), "{name}");
            }
        }
        let expected = radio_panels::evaluate_fixture(&fixture).unwrap()["result"].clone();
        let (result, markdown) = record.result().unwrap();
        assert_eq!(serde_json::to_value(&result).unwrap(), expected, "{name}");
        let reference: radio_panels::PanelResult = serde_json::from_value(expected).unwrap();
        assert_eq!(
            markdown,
            radio_panels::render_markdown(&reference).unwrap(),
            "{name}"
        );
        assert_eq!(record.view().outcome, result.outcome, "{name}");
        replayed += 1;
    }
    assert_eq!(replayed, 14);
}

#[test]
fn every_parse_fixture_is_taken_or_refused_with_the_fixed_phrase() {
    let (mut refused, mut taken) = (0, 0);
    for fixture in fixtures() {
        let input = &fixture["input"];
        if fixture["type"] != "parse" {
            continue;
        }
        let name = fixture["name"].as_str().unwrap();
        let crew: Vec<&str> = input["crew"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        let Ok(mut record) = Record::convene(
            input["kind"].as_str().unwrap(),
            input["n"].as_i64(),
            seats(&crew),
        ) else {
            continue;
        };
        record.state.round_no = input["round"].as_u64().unwrap() as u32 - 1;
        for id in input["known_drafts"].as_array().unwrap() {
            record
                .state
                .drafts
                .insert(id.as_str().unwrap().into(), "a draft".into());
        }
        let before = record.to_json().unwrap();
        let author = seat(input["author"].as_str().unwrap());
        let answer = record.turn(&author, input["post"].as_str().unwrap());
        if fixture["expect"]["malformed"] == true {
            assert_eq!(answer, Err(NOT_BLOCK.to_owned()), "{name}");
            assert_eq!(record.to_json().unwrap(), before, "{name}");
            refused += 1;
        } else {
            assert_eq!(answer, Ok(false), "{name}");
            let block: Block = serde_json::from_value(fixture["expect"]["block"].clone()).unwrap();
            assert_eq!(record.pending[&author.name], block, "{name}");
            taken += 1;
        }
    }
    assert_eq!((refused, taken), (24, 6));
}

const AGREE: &str = "```deliberation\nposition: agree\nitems: [\"oak\", \"elm\"]\n```";

#[test]
fn a_turn_by_anyone_but_a_seat_is_refused_and_changes_nothing() {
    let mut record = Record::convene("list", Some(2), seats(&["juniper", "fern"])).unwrap();
    let before = record.to_json().unwrap();
    for author in [
        seat("moss"),
        Author::member("scribe", "juniper"),
        Author::human("juniper"),
    ] {
        assert_eq!(record.turn(&author, AGREE), Err(NOT_SEAT.to_owned()));
    }
    assert_eq!(record.to_json().unwrap(), before);
}

#[test]
fn a_round_steps_only_once_every_seat_has_posted_and_a_seat_posts_once() {
    let mut record = Record::convene("list", Some(2), seats(&["juniper", "fern", "moss"])).unwrap();
    assert_eq!(record.result().unwrap_err(), NO_ROUND);
    assert_eq!(record.turn(&seat("juniper"), AGREE), Ok(false));
    assert_eq!(record.turn(&seat("juniper"), AGREE), Err(POSTED.to_owned()));
    assert_eq!(record.turn(&seat("fern"), AGREE), Ok(false));
    let view = record.view();
    assert_eq!((view.round, view.outcome.as_str()), (0, "open"));
    assert_eq!(
        view.posted
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>(),
        ["juniper", "fern"]
    );
    assert_eq!(record.result().unwrap_err(), NO_ROUND);
    assert_eq!(record.turn(&seat("moss"), AGREE), Ok(true));
    let view = record.view();
    assert_eq!((view.round, view.outcome.as_str()), (1, "agreed"));
    assert!(view.posted.is_empty());
    let (result, markdown) = record.result().unwrap();
    assert_eq!(
        result.answer,
        Some(radio_client::DeliberationAnswer::List(vec![
            "oak".into(),
            "elm".into()
        ]))
    );
    assert!(markdown.starts_with("# Deliberation result"));
    assert_eq!(record.turn(&seat("moss"), AGREE), Err(CLOSED.to_owned()));
    let reread = Record::from_json(&record.to_json().unwrap()).unwrap();
    assert_eq!(reread.view(), record.view());
}

#[test]
fn convene_checks_the_kind_size_and_seats_with_fixed_phrases() {
    let two = || seats(&["juniper", "fern"]);
    assert_eq!(
        Record::convene("vote", None, two()).unwrap_err(),
        "not a deliberation kind"
    );
    for n in [None, Some(0), Some(13)] {
        assert_eq!(Record::convene("list", n, two()).unwrap_err(), LIST_N);
    }
    assert_eq!(Record::convene("text", Some(2), two()).unwrap_err(), TEXT_N);
    let mut many: Vec<String> = (0..25).map(|i| format!("s{i}")).collect();
    for names in [
        vec!["juniper".to_owned()],
        vec!["juniper".to_owned(), "juniper".to_owned()],
        many.clone(),
    ] {
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            Record::convene("text", None, seats(&refs)).unwrap_err(),
            SEATS
        );
    }
    let same_name = vec![seat("juniper"), Author::member("scribe", "juniper")];
    assert_eq!(Record::convene("text", None, same_name).unwrap_err(), SEATS);
    for name in ["zoë", "o'hara", "a`b", "a#b"] {
        assert_eq!(
            Record::convene("text", None, seats(&["juniper", name])).unwrap_err(),
            SEAT_NAME
        );
    }
    many.truncate(24);
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert!(Record::convene("text", None, seats(&refs)).is_ok());
}

#[test]
fn every_seat_name_convene_takes_renders_a_dissenting_result() {
    let name = "Ab9 ._:/()-".repeat(6)[..64].to_owned();
    let mut record = Record::convene("list", Some(1), seats(&["juniper", &name])).unwrap();
    let object = |item: &str| {
        format!("```deliberation\nposition: object\nitems: [\"{item}\"]\nobjections: \"no\"\n```")
    };
    record.turn(&seat("juniper"), &object("oak")).unwrap();
    record.turn(&seat(&name), &object("elm")).unwrap();
    let (result, markdown) = record.result().unwrap();
    assert_eq!(result.dissent[0].member, name);
    assert!(markdown.contains(&format!("objections from {name}")));
}

#[test]
fn a_deliberation_takes_no_turn_after_its_round_limit() {
    let mut record = Record::convene("list", Some(1), seats(&["juniper", "fern"])).unwrap();
    record.state.round_no = ROUNDS_MAX;
    assert_eq!(
        record.turn(&seat("juniper"), AGREE),
        Err(ROUND_LIMIT.to_owned())
    );
}

const PROPOSE: &str = "```deliberation\nposition: propose\nitems: [\"oak\"]\n```";

#[test]
fn an_expired_time_box_steps_the_posted_seats_and_ends_open_with_a_result() {
    let mut record = Record::convene("list", Some(1), seats(&["juniper", "fern", "moss"]))
        .unwrap()
        .time_boxed(Some("15m"), 1000.0)
        .unwrap();
    assert_eq!(record.turn(&seat("juniper"), PROPOSE), Ok(false));
    assert!(!record.expire(1000.0 + 899.0));
    assert!(record.expire(1000.0 + 900.0));
    let view = record.view();
    assert_eq!(
        (
            view.round,
            view.outcome.as_str(),
            view.time_box.as_deref(),
            view.expired
        ),
        (1, "open", Some("15m"), true)
    );
    assert!(view.posted.is_empty());
    let (result, _) = record.result().unwrap();
    assert_eq!(
        (result.outcome.as_str(), result.trajectory.rounds),
        ("open", 1)
    );
    assert_eq!(record.turn(&seat("fern"), PROPOSE), Err(CLOSED.to_owned()));
    assert!(!record.expire(1000.0 + 3600.0));
    let reread = Record::from_json(&record.to_json().unwrap()).unwrap();
    assert_eq!(reread.view(), record.view());
}

#[test]
fn an_unexpired_or_absent_time_box_changes_nothing() {
    let mut boxed = Record::convene("list", Some(1), seats(&["juniper", "fern"]))
        .unwrap()
        .time_boxed(Some("1h"), 1000.0)
        .unwrap();
    boxed.turn(&seat("juniper"), PROPOSE).unwrap();
    let before = boxed.to_json().unwrap();
    assert!(!boxed.expire(1000.0 + 3599.0));
    assert_eq!(boxed.to_json().unwrap(), before);
    for word in [None, Some("deadlock")] {
        let mut record = Record::convene("list", Some(1), seats(&["juniper", "fern"]))
            .unwrap()
            .time_boxed(word, 1000.0)
            .unwrap();
        record.turn(&seat("juniper"), PROPOSE).unwrap();
        assert!(!record.expire(1000.0 + 1e9));
        assert!(!record.view().expired);
        assert_eq!(record.view().time_box.as_deref(), word);
        assert_eq!(record.turn(&seat("fern"), PROPOSE), Ok(true));
    }
}

#[test]
fn a_time_box_that_runs_out_before_any_turn_ends_with_no_round() {
    let mut record = Record::convene("text", None, seats(&["juniper", "fern"]))
        .unwrap()
        .time_boxed(Some("15m"), 0.0)
        .unwrap();
    assert!(record.expire(900.0));
    let view = record.view();
    assert_eq!(
        (view.round, view.outcome.as_str(), view.expired),
        (0, "open", true)
    );
    assert_eq!(record.result().unwrap_err(), NO_ROUND);
    assert_eq!(
        record.turn(
            &seat("juniper"),
            "```deliberation\nposition: propose\ndraft: \"oaks\"\n```"
        ),
        Err(CLOSED.to_owned())
    );
}

#[test]
fn an_agreed_deliberation_never_expires_and_an_unknown_time_box_is_refused() {
    let mut record = Record::convene("list", Some(2), seats(&["juniper", "fern"]))
        .unwrap()
        .time_boxed(Some("15m"), 0.0)
        .unwrap();
    record.turn(&seat("juniper"), AGREE).unwrap();
    record.turn(&seat("fern"), AGREE).unwrap();
    assert!(!record.expire(900.0));
    assert_eq!(
        (record.view().outcome.as_str(), record.view().expired),
        ("agreed", false)
    );
    for word in ["5m", "", "15M", "1h "] {
        assert_eq!(
            Record::convene("list", Some(1), seats(&["juniper", "fern"]))
                .unwrap()
                .time_boxed(Some(word), 0.0)
                .unwrap_err(),
            TIME_BOX
        );
    }
}

#[test]
fn a_record_written_before_time_boxes_still_reads() {
    let mut record = Record::convene("list", Some(1), seats(&["juniper", "fern"])).unwrap();
    record.turn(&seat("juniper"), PROPOSE).unwrap();
    let mut json: Value = serde_json::from_str(&record.to_json().unwrap()).unwrap();
    let object = json.as_object_mut().unwrap();
    for key in ["time_box", "convened_at", "expired"] {
        object.remove(key);
    }
    let mut old = Record::from_json(&json.to_string()).unwrap();
    assert!(!old.expire(1e12));
    assert_eq!(old.view(), record.view());
}
