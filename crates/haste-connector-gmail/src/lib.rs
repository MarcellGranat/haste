//! Gmail connector: OAuth, sync and plain-text drafts. It never sends and never modifies labels.

pub mod auth;
pub mod client;
pub mod connector;
pub mod consts;
pub mod errors;
pub mod keychain;
pub mod mapper;
pub mod mime;
pub mod testing;

pub use connector::{GmailConfig, GmailConnector};
