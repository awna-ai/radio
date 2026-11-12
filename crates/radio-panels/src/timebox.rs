use crate::{ProtocolError, sum::py_sum};
use serde::Serialize;
/// A supported time limit, including unlimited deliberation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeBox {
    FifteenMinutes,
    OneHour,
    Deadlock,
}
impl TimeBox {
    /// Parse a protocol keyword.
    pub fn parse(word: &str) -> Result<TimeBox, ProtocolError> {
        match word {
            "15m" => Ok(Self::FifteenMinutes),
            "1h" => Ok(Self::OneHour),
            "deadlock" => Ok(Self::Deadlock),
            _ => Err(ProtocolError::UnknownTimeBox),
        }
    }
}
/// A projection and the caller's ask, pause and expiry flags.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TimeboxDecision {
    pub projected_s: Option<f64>,
    pub ask: bool,
    pub pause: bool,
    pub expired: bool,
}
/// Decide from caller-supplied durations, distances and elapsed time.
pub fn timebox_decision(
    elapsed_s: f64,
    round_durations_s: &[f64],
    d_history: &[f64],
    time_box: Option<TimeBox>,
    asked: bool,
    answered: bool,
) -> TimeboxDecision {
    let projected_s = match (d_history.first(), d_history.last()) {
        (Some(first), Some(last)) if d_history.len() >= 2 && !round_durations_s.is_empty() => {
            let fall = (first - last) / (d_history.len() - 1) as f64;
            (fall > 0.0).then(|| {
                elapsed_s
                    + py_sum(round_durations_s.iter().copied()) / round_durations_s.len() as f64
                        * (last / fall)
            })
        }
        _ => None,
    };
    TimeboxDecision {
        projected_s,
        ask: time_box.is_none()
            && !asked
            && !d_history.is_empty()
            && (projected_s.is_none_or(|p| p > 300.0) || elapsed_s >= 300.0),
        pause: time_box.is_none() && !answered && elapsed_s >= 300.0,
        expired: (time_box == Some(TimeBox::FifteenMinutes) && elapsed_s >= 900.0)
            || (time_box == Some(TimeBox::OneHour) && elapsed_s >= 3600.0),
    }
}
