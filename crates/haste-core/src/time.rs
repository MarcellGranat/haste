//! Wall-clock helper kept in one place so tests can avoid it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
