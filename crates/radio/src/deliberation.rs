//! A deliberation: a room whose seats post turns, stepped in rounds with `radio-panels`.
//! Radio runs no app; whichever registered app lists a seat posts that seat's turn.

use radio_client::{Deliberation, DeliberationResult, Member};
use radio_panels::{
    Block, DeliberationState, Kind, Outcome, ProtocolError, RoundReport, TimeBox, build_result,
    parse_block, render_markdown, step_round, timebox_decision,
};
use radio_rooms::Author;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const NOT_DELIBERATION: &str = "not a deliberation";
pub(crate) const NOT_SEAT: &str = "not a seat";
pub(crate) const NOT_BLOCK: &str = "the turn holds no valid deliberation block";
pub(crate) const CLOSED: &str = "the deliberation is closed";
pub(crate) const POSTED: &str = "this seat has already posted in this round";
pub(crate) const ROUND_LIMIT: &str = "the deliberation has had its 50 rounds";
pub(crate) const NO_ROUND: &str = "no round has finished yet";
pub(crate) const TIME_BOX: &str = "a time box is 15m, 1h or deadlock";
const NOT_KIND: &str = "not a deliberation kind";
const LIST_N: &str = "a list deliberation needs n from 1 to 12";
const TEXT_N: &str = "a text deliberation takes no n";
const SEATS: &str = "a deliberation needs 2 to 24 seats with different names";
pub(crate) const SEAT_NAME: &str =
    "a seat's name may hold only letters, digits, spaces and . _ : / ( ) -";
const UNREADABLE: &str = "the deliberation could not be read";
const ROUNDS_MAX: u32 = 50;

/// What Radio keeps for a deliberation room, as JSON in the rooms store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    seats: Vec<Seat>,
    state: DeliberationState,
    /// The open round's blocks, by seat name.
    pending: BTreeMap<String, Block>,
    last: Option<RoundReport>,
    /// The time box asked at convene, as `TimeBox` parses it; absent means until an outcome.
    #[serde(default)]
    time_box: Option<String>,
    #[serde(default)]
    convened_at: f64,
    /// Whether the time box ran out while the deliberation was open; it then takes no turns.
    #[serde(default)]
    expired: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seat {
    kind: String,
    name: String,
}

impl Record {
    /// Checks the kind, size and seats; each seat is a member the caller has already checked.
    pub(crate) fn convene(kind: &str, n: Option<i64>, seats: Vec<Author>) -> Result<Self, String> {
        let kind = Kind::parse(kind).ok_or(NOT_KIND)?;
        // The result fences each dissenting seat's objections under a label holding its name.
        if !seats.iter().all(|seat| {
            seat.name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || " ._:/()-".contains(c))
        }) {
            return Err(SEAT_NAME.into());
        }
        let seats: Vec<Seat> = seats
            .into_iter()
            .map(|seat| Seat {
                kind: seat.kind,
                name: seat.name,
            })
            .collect();
        let crew = seats.iter().map(|seat| seat.name.clone()).collect();
        let state = DeliberationState::new(kind, n, crew).map_err(|error| match error {
            ProtocolError::InvalidState("list n must be 1..12") => LIST_N,
            ProtocolError::InvalidState("text n must be none") => TEXT_N,
            _ => SEATS,
        })?;
        Ok(Self {
            seats,
            state,
            pending: BTreeMap::new(),
            last: None,
            time_box: None,
            convened_at: 0.0,
            expired: false,
        })
    }

    /// Sets the time box asked at convene, counted from `now`.
    pub(crate) fn time_boxed(mut self, time_box: Option<&str>, now: f64) -> Result<Self, String> {
        if let Some(word) = time_box {
            TimeBox::parse(word).map_err(|_| TIME_BOX)?;
        }
        self.time_box = time_box.map(str::to_owned);
        self.convened_at = now;
        Ok(self)
    }

    /// Ends an open deliberation whose time box has run out at `now`: the open round steps with
    /// the seats that posted, if any, and the outcome stays as the step leaves it. Answers whether
    /// the record changed.
    pub(crate) fn expire(&mut self, now: f64) -> bool {
        let Some(time_box) = self
            .time_box
            .as_deref()
            .and_then(|w| TimeBox::parse(w).ok())
        else {
            return false;
        };
        if self.expired
            || self.state.closed.is_some()
            || !timebox_decision(
                now - self.convened_at,
                &[],
                &[],
                Some(time_box),
                false,
                false,
            )
            .expired
        {
            return false;
        }
        let mut next = self.clone();
        if !next.pending.is_empty() {
            let blocks = std::mem::take(&mut next.pending);
            let Ok(report) = step_round(&mut next.state, &blocks) else {
                return false;
            };
            next.last = Some(report);
        }
        next.expired = true;
        *self = next;
        true
    }

    pub(crate) fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|_| UNREADABLE.into())
    }

    pub(crate) fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|_| UNREADABLE.into())
    }

    /// Takes a seat's turn; answers whether it stepped the round. Nothing changes on an error.
    pub(crate) fn turn(&mut self, author: &Author, body: &str) -> Result<bool, String> {
        let seat = Seat {
            kind: author.kind.clone(),
            name: author.name.clone(),
        };
        if !self.seats.contains(&seat) {
            return Err(NOT_SEAT.into());
        }
        if self.state.closed.is_some() || self.expired {
            return Err(CLOSED.into());
        }
        if self.state.round_no >= ROUNDS_MAX {
            return Err(ROUND_LIMIT.into());
        }
        if self.pending.contains_key(&seat.name) {
            return Err(POSTED.into());
        }
        let known: BTreeSet<String> = self.state.drafts.keys().cloned().collect();
        let block = parse_block(
            body,
            self.state.kind,
            self.state.n,
            self.state.round_no + 1,
            &seat.name,
            &self.state.crew,
            &known,
        )
        .ok_or(NOT_BLOCK)?;
        let mut next = self.clone();
        next.pending.insert(seat.name, block);
        if next.pending.len() < next.seats.len() {
            *self = next;
            return Ok(false);
        }
        let blocks = std::mem::take(&mut next.pending);
        let report = step_round(&mut next.state, &blocks).map_err(|_| CLOSED)?;
        next.last = Some(report);
        *self = next;
        Ok(true)
    }

    pub(crate) fn view(&self) -> Deliberation {
        Deliberation {
            kind: match self.state.kind {
                Kind::List => "list",
                Kind::Text => "text",
            }
            .into(),
            n: self.state.n,
            seats: self.seats.iter().map(Seat::member).collect(),
            round: self.state.round_no,
            posted: self
                .seats
                .iter()
                .filter(|seat| self.pending.contains_key(&seat.name))
                .map(Seat::member)
                .collect(),
            outcome: outcome(self.state.closed).into(),
            time_box: self.time_box.clone(),
            expired: self.expired,
        }
    }

    /// The result and its markdown, once a round has been stepped.
    pub(crate) fn result(&self) -> Result<(DeliberationResult, String), String> {
        let last = self.last.as_ref().ok_or(NO_ROUND)?;
        let result = build_result(&self.state, last).map_err(|_| UNREADABLE)?;
        let markdown = render_markdown(&result).map_err(|_| UNREADABLE)?;
        let result = serde_json::to_value(&result)
            .and_then(serde_json::from_value)
            .map_err(|_| UNREADABLE)?;
        Ok((result, markdown))
    }
}

impl Seat {
    fn member(&self) -> Member {
        Member {
            kind: self.kind.clone(),
            name: self.name.clone(),
        }
    }
}

fn outcome(closed: Option<Outcome>) -> &'static str {
    match closed {
        None => "open",
        Some(Outcome::Agreed) => "agreed",
        Some(Outcome::Deadlock) => "deadlock",
    }
}

#[cfg(test)]
#[path = "deliberation_tests.rs"]
mod tests;
