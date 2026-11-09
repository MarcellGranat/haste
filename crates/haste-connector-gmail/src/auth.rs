//! OAuth 2.0 for Gmail. The flow itself lives in `haste-google-oauth`, shared with the Calendar
//! connector; this module fixes Gmail's scopes.

use std::time::Duration;

pub use haste_google_oauth::auth::{
    pkce_challenge, AuthError, ConsentResult, OAuthClient, OAuthEndpoints, TokenSource,
};

use crate::consts::SCOPES;

/// Run the consent flow for Gmail's two scopes (`gmail.readonly`, `gmail.compose`).
///
/// # Errors
/// See [`AuthError`].
pub async fn run_consent(
    endpoints: &OAuthEndpoints,
    client: &OAuthClient,
    http: &reqwest::Client,
    open_url: impl FnOnce(&str),
    timeout: Duration,
) -> Result<ConsentResult, AuthError> {
    haste_google_oauth::auth::run_consent(endpoints, client, &SCOPES, http, open_url, timeout).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_asks_for_exactly_readonly_and_compose() {
        assert_eq!(
            SCOPES,
            [
                "https://www.googleapis.com/auth/gmail.readonly",
                "https://www.googleapis.com/auth/gmail.compose"
            ]
        );
        assert!(SCOPES.iter().all(|s| !s.contains("gmail.modify")));
    }
}
