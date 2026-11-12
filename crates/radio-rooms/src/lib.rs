//! Rooms, messages, status and an outgoing queue in a private SQLite store.
//! The daemon is its one writer; every error is a fixed phrase, never a message body or status line.

mod private_dir;
mod store;
mod text;

/// Directory creation and permissions for private state.
pub use private_dir::private_dir;
/// The rooms store, its records, and its limits.
pub use store::*;
