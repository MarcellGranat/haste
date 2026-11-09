//! Constants for the Calendar connector (all tunables live here).

use std::time::Duration;

/// Scopes: read and approved-write of events, plus listing the human's calendars (the spike
/// showed `calendar.events` alone cannot call `calendarList.list`).
pub const SCOPE_EVENTS: &str = "https://www.googleapis.com/auth/calendar.events";
pub const SCOPE_CALENDAR_LIST: &str =
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly";
pub const SCOPES: [&str; 2] = [SCOPE_EVENTS, SCOPE_CALENDAR_LIST];

/// A separate Keychain service from Gmail's `com.haste.gmail`.
pub const KEYCHAIN_SERVICE: &str = "com.haste.gcal";
/// Keychain account holding the OAuth client (id and secret), apart from the refresh token.
pub const CLIENT_ACCOUNT: &str = "oauth-client";

pub const API_BASE: &str = "https://www.googleapis.com/calendar/v3";
pub const GCAL_SYNC_INTERVAL_SECS: u64 = 300;
/// Only events ending within this many days in the past are stored.
pub const CAL_BACKFILL_DAYS: i64 = 30;
/// Only events starting within this many days ahead are stored.
pub const CAL_LOOKAHEAD_DAYS: i64 = 180;
pub const PAGE_SIZE: u32 = 250;
/// Pages read per calendar per cycle (a safety bound on one cycle).
pub const MAX_PAGES: usize = 40;
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);
pub const DAY_SECS: i64 = 86_400;
