//! Apple Reminders connector. A small Swift helper (EventKit) does the real work; this crate
//! speaks a typed, closed JSON protocol to it and exposes the result as a haste `Connector`.

pub mod connector;
pub mod protocol;
pub mod runner;
pub mod settings;

pub use connector::{CycleAudit, RemindersConnector, RemindersCursor};
pub use protocol::{AuthStatus, RemindersRequest, RemindersResponse, WireReminder};
pub use runner::{Helper, HelperError, ProcessHelper};
pub use settings::Settings;
