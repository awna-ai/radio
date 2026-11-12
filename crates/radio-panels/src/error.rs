/// A protocol validation or replay failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidState(&'static str), // the reference's ValueError on a bad kind, n or member list
    Closed,                     // step_round on a closed panel
    UnknownTimeBox,
    InvalidFenceLabel,
    ListNeedsN,
    BadDraftId,      // a draft id whose text after the last ".r" is not an integer
    Fixture(String), // a vector the replay cannot read
}
impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidState(reason) => f.write_str(reason),
            Self::Closed => f.write_str("reopen a closed deliberation before stepping"),
            Self::UnknownTimeBox => f.write_str("unknown time box"),
            Self::InvalidFenceLabel => f.write_str("invalid fence label"),
            Self::ListNeedsN => f.write_str("list distance requires n"),
            Self::BadDraftId => f.write_str("invalid draft id"),
            Self::Fixture(reason) => write!(f, "invalid fixture: {reason}"),
        }
    }
}
impl std::error::Error for ProtocolError {}
