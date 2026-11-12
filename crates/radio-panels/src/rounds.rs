use crate::{Block, Kind, Position, ProtocolError, clean, distance_matrix, distances, match_key};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
/// A mechanical stopping outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Agreed,
    Deadlock,
}

/// A list answer or text; reports use text for a draft id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Answer {
    List(Vec<String>),
    Text(String),
}

/// Serializable panel history and carried round state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliberationState {
    pub kind: Kind,
    #[serde(default)]
    pub n: Option<i64>,
    pub crew: Vec<String>,
    #[serde(default)]
    pub round_no: u32,
    pub last_block: BTreeMap<String, Option<Block>>,
    pub weight: BTreeMap<String, f64>,
    pub holdout: BTreeMap<String, bool>,
    pub stall: BTreeMap<String, u32>,
    pub prev_own: BTreeMap<String, f64>,
    #[serde(rename = "prev_D", default)]
    pub prev_d: Option<f64>,
    #[serde(default)]
    pub no_progress: u32,
    #[serde(default)]
    pub closed: Option<Outcome>,
    pub d_history: Vec<f64>,
    #[serde(default)]
    pub drafts: BTreeMap<String, String>,
}
impl DeliberationState {
    /// Create a panel with validated kind, size and members.
    pub fn new(kind: Kind, n: Option<i64>, crew: Vec<String>) -> Result<Self, ProtocolError> {
        validate(kind, n, &crew)?;
        let mut state = Self {
            kind,
            n,
            crew,
            round_no: 0,
            last_block: BTreeMap::new(),
            weight: BTreeMap::new(),
            holdout: BTreeMap::new(),
            stall: BTreeMap::new(),
            prev_own: BTreeMap::new(),
            prev_d: None,
            no_progress: 0,
            closed: None,
            d_history: Vec::new(),
            drafts: BTreeMap::new(),
        };
        state.fill_members();
        Ok(state)
    }
    /// Serialize an independent state snapshot.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
    /// Deserialize, validate and fill missing member entries.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, ProtocolError> {
        let mut state: Self = serde_json::from_value(value.clone())
            .map_err(|_| ProtocolError::InvalidState("invalid state json"))?;
        validate(state.kind, state.n, &state.crew)?;
        state.fill_members();
        Ok(state)
    }
    fn fill_members(&mut self) {
        for name in &self.crew {
            self.last_block.entry(name.clone()).or_insert(None);
            self.weight.entry(name.clone()).or_insert(1.0);
            self.holdout.entry(name.clone()).or_insert(false);
            self.stall.entry(name.clone()).or_insert(0);
            self.prev_own.entry(name.clone()).or_insert(1.0);
        }
    }
}

/// Measurements and stopping decision for one round.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundReport {
    pub round: u32,
    #[serde(rename = "D")]
    pub d: f64,
    pub progress: bool,
    pub no_progress: u32,
    pub own: BTreeMap<String, f64>,
    pub weights: BTreeMap<String, f64>,
    pub next_weights: BTreeMap<String, f64>,
    pub holdouts: Vec<String>,
    pub agreed: bool,
    pub outcome: Option<Outcome>,
    pub agreed_answer: Option<Answer>,
}
/// Carry fresh blocks forward and advance agreement, holdouts and stopping.
pub fn step_round(
    state: &mut DeliberationState,
    blocks: &BTreeMap<String, Block>,
) -> Result<RoundReport, ProtocolError> {
    if state.closed.is_some() {
        return Err(ProtocolError::Closed);
    }
    state.round_no = state
        .round_no
        .checked_add(1)
        .ok_or(ProtocolError::InvalidState("round number overflow"))?;
    state.fill_members();
    for name in &state.crew {
        if let Some(block) = blocks.get(name) {
            state.last_block.insert(name.clone(), Some(block.clone()));
            if let (Some(id), Some(draft)) = (&block.draft_id, &block.draft) {
                state.drafts.insert(id.clone(), clean(draft, 1500));
            }
        }
    }
    let weights = state.weight.clone();
    let matrix = distance_matrix(state.kind, state.n, &state.crew, &state.last_block)?;
    let (own, d) = distances(&state.crew, &matrix, &weights)?;
    let progress = state.round_no > 1 && state.prev_d.is_some_and(|prev| d < prev - 1e-12);
    if state.round_no > 1 {
        state.no_progress = if progress {
            0
        } else {
            state.no_progress.saturating_add(1)
        };
    }
    if state.crew.len() >= 3 && state.round_no > 1 {
        for name in &state.crew {
            let fell = own[name] < state.prev_own[name] - 1e-12;
            if state.holdout[name] {
                if fell {
                    state.holdout.insert(name.clone(), false);
                    state.stall.insert(name.clone(), 0);
                    state.weight.insert(name.clone(), 1.0);
                } else {
                    let weight = state.weight[name] * 0.5;
                    state
                        .weight
                        .insert(name.clone(), if weight < 0.1 { 0.0 } else { weight });
                }
            } else {
                let stall = if progress && !fell {
                    state.stall[name].saturating_add(1)
                } else {
                    0
                };
                state.stall.insert(name.clone(), stall);
                state.holdout.insert(name.clone(), stall >= 2);
                state
                    .weight
                    .insert(name.clone(), if stall >= 2 { 0.5 } else { 1.0 });
            }
        }
    }
    state.prev_own = own.clone();
    state.prev_d = Some(d);
    state.d_history.push(d);
    let answer = agreement(state, blocks);
    let agreed = answer.is_some();
    let outcome = if agreed {
        Some(Outcome::Agreed)
    } else if state.no_progress >= 4 {
        Some(Outcome::Deadlock)
    } else {
        None
    };
    state.closed = outcome;
    let mut holdouts: Vec<_> = state
        .crew
        .iter()
        .filter(|n| state.holdout[*n])
        .cloned()
        .collect();
    holdouts.sort();
    Ok(RoundReport {
        round: state.round_no,
        d,
        progress,
        no_progress: state.no_progress,
        own,
        weights,
        next_weights: state.weight.clone(),
        holdouts,
        agreed,
        outcome,
        agreed_answer: answer,
    })
}
/// Reopen while retaining prior distances, drafts and posts.
pub fn reopen(state: &mut DeliberationState) {
    state.closed = None;
    state.no_progress = 0;
    for name in &state.crew {
        state.holdout.insert(name.clone(), false);
        state.stall.insert(name.clone(), 0);
        state.weight.insert(name.clone(), 1.0);
    }
}

fn validate(kind: Kind, n: Option<i64>, crew: &[String]) -> Result<(), ProtocolError> {
    if kind == Kind::List && !n.is_some_and(|n| (1..=12).contains(&n)) {
        return Err(ProtocolError::InvalidState("list n must be 1..12"));
    }
    if kind == Kind::Text && n.is_some() {
        return Err(ProtocolError::InvalidState("text n must be none"));
    }
    if !(2..=24).contains(&crew.len())
        || crew.iter().any(String::is_empty)
        || crew.iter().collect::<BTreeSet<_>>().len() != crew.len()
    {
        return Err(ProtocolError::InvalidState(
            "members must contain 2..24 unique nonempty names",
        ));
    }
    Ok(())
}

fn agreement(state: &DeliberationState, fresh: &BTreeMap<String, Block>) -> Option<Answer> {
    let names: Vec<_> = state.crew.iter().filter(|n| !state.holdout[*n]).collect();
    if names.len() * 2 <= state.crew.len() {
        return None;
    }
    let blocks: Option<Vec<_>> = names
        .iter()
        .map(|name| fresh.get(*name).filter(|b| b.position == Position::Agree))
        .collect();
    let blocks = blocks?;
    let first = blocks.first()?;
    if state.kind == Kind::List {
        let keys: BTreeSet<_> = first.items.iter().map(|s| match_key(s)).collect();
        if Some(keys.len() as i64) == state.n
            && blocks.iter().all(|b| {
                b.items
                    .iter()
                    .map(|s| match_key(s))
                    .collect::<BTreeSet<_>>()
                    == keys
            })
        {
            return Some(Answer::List(first.items.clone()));
        }
    } else if first.agree_with.is_some() && blocks.iter().all(|b| b.agree_with == first.agree_with)
    {
        return first.agree_with.clone().map(Answer::Text);
    }
    None
}
