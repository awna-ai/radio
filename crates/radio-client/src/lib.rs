//! The client side of the Radio's socket contract, CONTRACT.md at the repo's root.
//! The contract's numbers live here as constants; a test checks them against CONTRACT.md.

/// The protocol major every request carries as `"v"`.
pub const PROTOCOL_MAJOR: u32 = 1;
/// The protocol minor of major 1 this crate speaks.
pub const PROTOCOL_MINOR: u32 = 0;
