//! Beeper connector: WhatsApp, Messenger and iMessage chats through the Beeper Desktop API, a
//! local HTTP API inside Beeper Desktop. Reads only the chats the human selected, on-device
//! connections only; a reply goes out only after a human confirmed it (agents never send).
//!
//! The API's field names are not confirmed until the live spike (design Q1-Q6), so everything
//! that depends on them lives in [`wire`], behind one provisional layer.

pub mod client;
pub mod config;
pub mod connector;
pub mod consts;
pub mod mapper;
pub mod wire;

pub use config::{AccountSelection, BeeperConfig, ChatSelection};
pub use connector::{BeeperConnector, BeeperSource, CycleAudit};
