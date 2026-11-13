//! Tunables and endpoints in one place. None of these is configurable at runtime.

use std::time::Duration;

/// How far back the first sync looks.
pub const BACKFILL_DAYS: u32 = 30;
/// Upper bound on threads fetched by one backfill, however many match.
pub const BACKFILL_MAX_THREADS: usize = 200;
/// Pause between two thread reads, so a backfill stays far below Gmail's per-user rate limit.
pub const THREAD_GET_PACING: Duration = Duration::from_millis(120);
/// A rate-limited thread read is retried this many times, doubling the wait from the base.
pub const THREAD_RETRIES: u32 = 3;
pub const THREAD_RETRY_BASE: Duration = Duration::from_secs(1);
/// Stored plain-text body limit, in bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Seconds between scheduled syncs.
pub const GMAIL_SYNC_INTERVAL_SECS: u64 = 300;

pub const SCOPE_READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";
pub const SCOPE_COMPOSE: &str = "https://www.googleapis.com/auth/gmail.compose";
/// Exactly these two scopes; nothing that can modify, archive or delete.
pub const SCOPES: [&str; 2] = [SCOPE_READONLY, SCOPE_COMPOSE];

pub const KEYCHAIN_SERVICE: &str = "com.haste.gmail";
pub const API_BASE: &str = "https://gmail.googleapis.com";
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Prefix of `body_text` for messages that have no `text/plain` part.
pub const BODY_UNAVAILABLE_MARKER: &str = "[body unavailable] ";
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);
/// Access tokens are refreshed this long before they expire.
pub const TOKEN_EXPIRY_MARGIN: Duration = Duration::from_secs(60);
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
