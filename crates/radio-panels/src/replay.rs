use crate::{
    Block, DeliberationState, Kind, Position, ProtocolError, TimeBox, build_result, parse_block,
    reopen, step_round, timebox_decision,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn read<T: DeserializeOwned>(value: &Value) -> Result<T, ProtocolError> {
    serde_json::from_value(value.clone())
        .map_err(|_| ProtocolError::Fixture("field has the wrong shape".to_owned()))
}

#[derive(Deserialize)]
struct ParseInput {
    post: String,
    kind: String,
    n: Option<i64>,
    round: u32,
    author: String,
    crew: Vec<String>,
    known_drafts: BTreeSet<String>,
}
#[derive(Deserialize)]
struct TimeInput {
    elapsed_s: f64,
    round_durations_s: Vec<f64>,
    d_history: Vec<f64>,
    time_box: Option<String>,
    asked: bool,
    answered: bool,
}
#[derive(Deserialize)]
struct RoundsInput {
    kind: Kind,
    n: Option<i64>,
    crew: Vec<String>,
    rounds: Vec<BTreeMap<String, Option<FixtureBlock>>>,
    reopen_after: Option<u32>,
}
#[derive(Deserialize)]
struct FixtureBlock {
    position: Position,
    #[serde(default)]
    items: Vec<String>,
    draft: Option<String>,
    agree_with: Option<String>,
    #[serde(default)]
    ratings: BTreeMap<String, i64>,
    #[serde(default)]
    objections: String,
}

/// Evaluate a portable protocol fixture entirely in memory.
pub fn evaluate_fixture(fixture: &Value) -> Result<Value, ProtocolError> {
    let fail = |reason: &str| ProtocolError::Fixture(reason.to_owned());
    let inputs = fixture.get("input").ok_or_else(|| fail("missing input"))?;
    match fixture.get("type").and_then(Value::as_str) {
        Some("parse") => {
            let input: ParseInput = read(inputs)?;
            let block = Kind::parse(&input.kind).and_then(|kind| {
                parse_block(
                    &input.post,
                    kind,
                    input.n,
                    input.round,
                    &input.author,
                    &input.crew,
                    &input.known_drafts,
                )
            });
            Ok(match block {
                Some(block) => json!({"block": block}),
                None => json!({"malformed": true}),
            })
        }
        Some("timebox") => {
            let input: TimeInput = read(inputs)?;
            let time_box = input.time_box.as_deref().map(TimeBox::parse).transpose()?;
            Ok(json!(timebox_decision(
                input.elapsed_s,
                &input.round_durations_s,
                &input.d_history,
                time_box,
                input.asked,
                input.answered
            )))
        }
        Some("rounds") => {
            let input: RoundsInput = read(inputs)?;
            let mut state = DeliberationState::new(input.kind, input.n, input.crew)
                .map_err(|e| fail(&e.to_string()))?;
            let mut reports = Vec::new();
            let mut last = None;
            for (index, turn) in input.rounds.into_iter().enumerate() {
                let round = u32::try_from(index + 1).map_err(|_| fail("too many rounds"))?;
                let blocks = turn
                    .into_iter()
                    .filter_map(|(name, fields)| {
                        fields.map(|fields| {
                            let draft_id =
                                fields.draft.as_ref().map(|_| format!("{name}.r{round}"));
                            (
                                name,
                                Block {
                                    position: fields.position,
                                    items: fields.items,
                                    draft: fields.draft,
                                    draft_id,
                                    agree_with: fields.agree_with,
                                    ratings: fields.ratings,
                                    objections: fields.objections,
                                },
                            )
                        })
                    })
                    .collect();
                let report = step_round(&mut state, &blocks)?;
                let mut fields = json!(report);
                if let Some(fields) = fields.as_object_mut() {
                    fields.remove("agreed_answer");
                }
                reports.push(fields);
                last = Some(report);
                if input.reopen_after == Some(round) {
                    reopen(&mut state);
                }
            }
            let last = last.ok_or_else(|| fail("round fixture must contain at least one round"))?;
            Ok(json!({"rounds": reports, "result": build_result(&state, &last)?}))
        }
        _ => Err(fail("unknown fixture type")),
    }
}
