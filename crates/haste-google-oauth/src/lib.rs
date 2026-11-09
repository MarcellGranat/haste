//! Google OAuth for installed apps, shared by the Gmail and Calendar connectors: PKCE with a
//! loopback redirect, access-token refresh, a Keychain-backed refresh-token store and the
//! mapping of Google error responses. Each connector brings its own scopes and Keychain
//! service, so their tokens never mix.

pub mod auth;
pub mod errors;
pub mod keychain;
pub mod testing;

use std::time::Duration;

/// Google's authorization endpoint.
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Google's token endpoint.
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Refresh the access token this long before it expires.
pub const TOKEN_EXPIRY_MARGIN: Duration = Duration::from_secs(60);
