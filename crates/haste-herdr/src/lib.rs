//! Talking to herdr: a local unix socket (snapshot plus live events) and, for remote machines,
//! the hub's own `herdr --machine <label>` CLI (polled snapshots).
//!
//! Everything here is read-only apart from `agent.prompt` result messages. There is no way to
//! construct a request that reads pane output, sends keys, or changes herdr's layout.

pub mod backoff;
pub mod command;
pub mod diff;
pub mod local;
pub mod prompt;
pub mod protocol;
pub mod remote;

pub use protocol::{FeedError, HERDR_PROTOCOL};
