//! iCloud Mail connector: read-only IMAP over implicit TLS with an app-specific password
//! kept in the Keychain. It never writes to the mailbox and never sends.

pub mod auth;
pub mod config;
pub mod connector;
pub mod parse;
pub mod secrets;
pub mod session;
pub mod sync;
pub mod testing;

pub use config::{FolderConfig, IcloudConfig};
pub use connector::{CycleAudit, IcloudConnector};
