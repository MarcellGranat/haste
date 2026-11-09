//! Google Calendar connector: reads events incrementally, and creates or updates an event only
//! after a human approved a proposal, always with `sendUpdates=none`. It can neither delete an
//! event nor change attendees, and it shares OAuth code (not tokens) with the Gmail connector.

pub mod client;
pub mod config;
pub mod connector;
pub mod consts;
pub mod mapper;
pub mod timefmt;

pub use config::{ClientCredentials, GcalConfig};
pub use connector::{CycleAudit, GcalConnector};
