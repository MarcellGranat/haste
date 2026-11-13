//! Mapping of provider responses to `ConnectorError`. Fails closed on anything unknown.

use haste_core::connector::ConnectorError;

const RATE_LIMIT_MARKERS: [&str; 4] = [
    "ratelimitexceeded",
    "userratelimitexceeded",
    "quotaexceeded",
    "backenderror",
];
const CHALLENGE_MARKERS: [&str; 5] = ["challenge", "captcha", "reauth", "verify it", "suspicious"];

/// What kind of failure a request error was, as a fixed word (never the error text or URL).
#[must_use]
pub fn request_kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else {
        "request"
    }
}

/// Classify a non-success HTTP response.
///
/// * 429 and 5xx, and 403 rate-limit reasons, are `Transient` (bounded retries).
/// * `invalid_grant` is `Checkpoint`: the human must re-authorize; no retry.
/// * Challenge-like bodies are `Challenge`; both are terminal and trip the breaker.
/// * 400, 401 and 404 are our own mistakes or missing objects: `Other`.
/// * Anything else, including a 403 with an unrecognised body, is `Checkpoint` (fail closed).
#[must_use]
pub fn classify(status: u16, body: &str) -> ConnectorError {
    let lower = body.to_lowercase();
    if status == 429 || (500..600).contains(&status) {
        return ConnectorError::Transient(format!("http {status}"));
    }
    if lower.contains("invalid_grant") {
        return ConnectorError::Checkpoint("re-authorization required (invalid_grant)".into());
    }
    if status == 403 && RATE_LIMIT_MARKERS.iter().any(|m| lower.contains(m)) {
        return ConnectorError::Transient("rate limited".into());
    }
    if CHALLENGE_MARKERS.iter().any(|m| lower.contains(m)) {
        return ConnectorError::Challenge(format!("http {status}: verification requested"));
    }
    match status {
        400 | 401 | 404 => ConnectorError::Other(format!("http {status}")),
        _ => {
            ConnectorError::Checkpoint(format!("http {status}: unrecognised restriction response"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn rate_limits_and_server_errors_are_transient() {
        assert!(matches!(classify(429, "{}"), ConnectorError::Transient(_)));
        assert!(matches!(classify(503, ""), ConnectorError::Transient(_)));
        assert!(matches!(
            classify(403, &fixture("error_rate_limit.json")),
            ConnectorError::Transient(_)
        ));
    }

    #[test]
    fn invalid_grant_is_terminal() {
        assert!(matches!(
            classify(400, &fixture("error_invalid_grant.json")),
            ConnectorError::Checkpoint(_)
        ));
    }

    #[test]
    fn challenge_and_restriction_bodies_open_the_breaker() {
        assert!(matches!(
            classify(403, &fixture("error_challenge.json")),
            ConnectorError::Challenge(_)
        ));
        assert!(matches!(
            classify(403, &fixture("error_account_disabled.json")),
            ConnectorError::Checkpoint(_)
        ));
    }

    #[test]
    fn unknown_forbidden_body_fails_closed() {
        assert!(matches!(
            classify(403, "<html>nope</html>"),
            ConnectorError::Checkpoint(_)
        ));
        assert!(matches!(classify(418, ""), ConnectorError::Checkpoint(_)));
    }

    #[test]
    fn our_own_mistakes_are_not_restrictions() {
        for status in [400, 401, 404] {
            assert!(
                matches!(classify(status, "{}"), ConnectorError::Other(_)),
                "{status}"
            );
        }
    }
}
