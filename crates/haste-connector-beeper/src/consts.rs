//! Constants for the Beeper connector. Limits are fixed in code on purpose: an agent (or a
//! config file it could edit) cannot raise them.

use std::time::Duration;

/// Beeper Desktop's local API.
pub const BEEPER_BASE_URL: &str = "http://127.0.0.1:23373";
/// Keychain service and account of the API token (never written to a file or an env variable).
pub const KEYCHAIN_SERVICE: &str = "com.haste.beeper";
pub const TOKEN_ACCOUNT: &str = "api-token";

pub const BEEPER_POLL_SECS: u64 = 300;
pub const BEEPER_PAGE_SIZE: usize = 50;
pub const BEEPER_MAX_PAGES_PER_CHAT: usize = 4;
pub const BEEPER_MAX_CHATS_PER_CYCLE: usize = 50;
/// Newest messages read for a chat the first time.
pub const BEEPER_BACKFILL_PER_CHAT: usize = 200;
/// Opening a chat reads up to this many messages older than the stored ones, within this many
/// pages counted from the newest.
pub const BEEPER_HISTORY_EXTRA: usize = 150;
pub const BEEPER_HISTORY_MAX_PAGES: usize = 10;
/// Pages walked when the human asks for older messages (no date limit).
pub const BEEPER_HISTORY_DEEP_PAGES: usize = 60;
/// A message body is stored cut to this many bytes (at a character boundary).
pub const BEEPER_BODY_MAX_BYTES: usize = 8 * 1024;
pub const BEEPER_DEFAULT_HISTORY_DAYS: u32 = 30;
pub const BEEPER_MAX_HISTORY_DAYS: u32 = 90;
pub const BEEPER_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest reply text accepted for a send.
pub const BEEPER_MAX_SEND_BYTES: usize = 4 * 1024;
pub const DAY_SECS: i64 = 86_400;
/// Stored in place of a non-text message; never the attachment's name.
pub const ATTACHMENT_LABEL: &str = "[attachment]";
pub const TRUNCATED_MARKER: &str = " [truncated]";
