//! iCloud Mail connector: IMAP over implicit TLS with an app-specific password kept in the
//! Keychain. It reads. The only things it ever writes are a human-approved send (SMTP) and the
//! copy of that message filed in the Sent folder; agents have no path to either.

pub mod auth;
pub mod config;
pub mod connector;
pub mod parse;
pub mod secrets;
pub mod session;
pub mod smtp;
pub mod sync;
pub mod testing;

pub use config::{FolderConfig, IcloudConfig};
pub use connector::{CycleAudit, IcloudConnector};
