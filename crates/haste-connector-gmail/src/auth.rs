//! OAuth 2.0 for an installed app: PKCE, loopback redirect, token refresh.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use haste_core::connector::ConnectorError;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::consts::{SCOPES, TOKEN_EXPIRY_MARGIN};
use crate::errors::classify;
use crate::keychain::TokenStore;

const REQUEST_LIMIT_BYTES: usize = 8 * 1024;
const VERIFIER_BYTES: usize = 32;
const STATE_BYTES: usize = 16;

#[derive(Debug, Clone)]
pub struct OAuthEndpoints {
    pub auth_url: String,
    pub token_url: String,
}

#[derive(Debug, Clone)]
pub struct OAuthClient {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug)]
pub enum AuthError {
    Timeout,
    Denied(String),
    StateMismatch,
    Network(String),
    Exchange(String),
    NoRefreshToken,
    Io(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "timed out waiting for the browser"),
            Self::Denied(m) => write!(f, "consent was not granted: {m}"),
            Self::StateMismatch => write!(f, "redirect state did not match; aborting"),
            Self::Network(m) | Self::Exchange(m) | Self::Io(m) => write!(f, "{m}"),
            Self::NoRefreshToken => write!(
                f,
                "Google returned no refresh token; revoke the app and try again"
            ),
        }
    }
}

impl std::error::Error for AuthError {}

/// `base64url(sha256(verifier))`, per RFC 7636 section 4.2.
#[must_use]
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_urlsafe(bytes: usize) -> String {
    let mut buf = vec![0_u8; bytes];
    getrandom::getrandom(&mut buf).expect("OS randomness unavailable");
    URL_SAFE_NO_PAD.encode(buf)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// Holds the access token in memory only; the refresh token stays in the [`TokenStore`].
pub struct TokenSource {
    http: reqwest::Client,
    token_url: String,
    client: OAuthClient,
    account: String,
    store: Arc<dyn TokenStore>,
    cache: Mutex<Option<(String, Instant)>>,
}

impl TokenSource {
    #[must_use]
    pub fn new(
        http: reqwest::Client,
        token_url: &str,
        client: OAuthClient,
        account: &str,
        store: Arc<dyn TokenStore>,
    ) -> Self {
        Self {
            http,
            token_url: token_url.to_owned(),
            client,
            account: account.to_owned(),
            store,
            cache: Mutex::new(None),
        }
    }

    /// A valid access token, refreshing from the stored refresh token when needed.
    ///
    /// # Errors
    /// `Checkpoint` when the refresh token is missing or rejected (`invalid_grant`): the
    /// guard then engages the kill switch and the human re-authorizes. `Transient` for
    /// network problems and 5xx.
    pub async fn access_token(&self) -> Result<String, ConnectorError> {
        let mut cache = self.cache.lock().await;
        if let Some((token, expires)) = cache.as_ref() {
            if Instant::now() < *expires {
                return Ok(token.clone());
            }
        }
        let refresh = self
            .store
            .get(&self.account)
            .map_err(ConnectorError::Other)?
            .ok_or_else(|| {
                ConnectorError::Checkpoint("no refresh token stored; run the auth verb".into())
            })?;
        let response = self
            .http
            .post(&self.token_url)
            .form(&[
                ("client_id", self.client.client_id.as_str()),
                ("client_secret", self.client.client_secret.as_str()),
                ("refresh_token", refresh.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(|e| {
                ConnectorError::Transient(format!(
                    "token endpoint unreachable: {}",
                    e.without_url()
                ))
            })?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        if status != 200 {
            return Err(classify(status, &body));
        }
        let parsed: TokenResponse = serde_json::from_str(&body)
            .map_err(|_| ConnectorError::Other("malformed token response".into()))?;
        let lifetime = Duration::from_secs(parsed.expires_in).saturating_sub(TOKEN_EXPIRY_MARGIN);
        *cache = Some((parsed.access_token.clone(), Instant::now() + lifetime));
        Ok(parsed.access_token)
    }
}

/// What a successful consent produces.
pub struct ConsentResult {
    pub refresh_token: String,
    pub access_token: String,
}

fn consent_url(
    endpoints: &OAuthEndpoints,
    client: &OAuthClient,
    redirect: &str,
    challenge: &str,
    state: &str,
) -> String {
    let scopes = SCOPES.join(" ");
    let mut url = reqwest::Url::parse(&endpoints.auth_url).expect("valid auth url");
    url.query_pairs_mut()
        .append_pair("client_id", &client.client_id)
        .append_pair("redirect_uri", redirect)
        .append_pair("response_type", "code")
        .append_pair("scope", &scopes)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state);
    url.into()
}

/// Run the consent flow: bind a one-shot listener on `127.0.0.1`, hand the consent URL to
/// `open_url`, accept exactly one redirect (or time out), then exchange the code with PKCE.
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
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| AuthError::Io(e.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|e| AuthError::Io(e.to_string()))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}");
    let verifier = random_urlsafe(VERIFIER_BYTES);
    let state = random_urlsafe(STATE_BYTES);
    open_url(&consent_url(
        endpoints,
        client,
        &redirect,
        &pkce_challenge(&verifier),
        &state,
    ));

    let code = tokio::time::timeout(timeout, wait_for_redirect(&listener, &state))
        .await
        .map_err(|_| AuthError::Timeout)??;
    drop(listener);

    let response = http
        .post(&endpoints.token_url)
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("code", code.as_str()),
            ("code_verifier", verifier.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect.as_str()),
        ])
        .send()
        .await
        .map_err(|e| AuthError::Network(e.without_url().to_string()))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AuthError::Exchange(format!(
            "token exchange failed with http {}",
            status.as_u16()
        )));
    }
    let parsed: TokenResponse = serde_json::from_str(&body)
        .map_err(|_| AuthError::Exchange("malformed token response".into()))?;
    Ok(ConsentResult {
        refresh_token: parsed.refresh_token.ok_or(AuthError::NoRefreshToken)?,
        access_token: parsed.access_token,
    })
}

/// Accept one connection, answer it, and return the authorization code.
async fn wait_for_redirect(
    listener: &TcpListener,
    expected_state: &str,
) -> Result<String, AuthError> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|e| AuthError::Io(e.to_string()))?;
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < REQUEST_LIMIT_BYTES {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| AuthError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    let target = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/");
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|e| AuthError::Io(e.to_string()))?;
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }
    let page = if code.is_some() && error.is_none() {
        "haste: authorization received. You can close this tab."
    } else {
        "haste: authorization failed. You can close this tab."
    };
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
    if let Some(e) = error {
        return Err(AuthError::Denied(e));
    }
    if state.as_deref() != Some(expected_state) {
        return Err(AuthError::StateMismatch);
    }
    code.ok_or_else(|| AuthError::Denied("no code in redirect".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_7636_vector() {
        // RFC 7636 appendix B.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn consent_url_requests_exactly_the_two_scopes_with_pkce() {
        let url = consent_url(
            &OAuthEndpoints {
                auth_url: crate::consts::AUTH_URL.into(),
                token_url: crate::consts::TOKEN_URL.into(),
            },
            &OAuthClient {
                client_id: "cid".into(),
                client_secret: "s".into(),
            },
            "http://127.0.0.1:9",
            "chal",
            "st",
        );
        let parsed = reqwest::Url::parse(&url).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(
            q["scope"],
            format!(
                "{} {}",
                crate::consts::SCOPE_READONLY,
                crate::consts::SCOPE_COMPOSE
            )
        );
        assert_eq!(q["code_challenge_method"], "S256");
        assert!(q["redirect_uri"].starts_with("http://127.0.0.1:"));
        assert!(!url.contains("gmail.modify") && !url.contains("mail.google.com/"));
    }

    #[test]
    fn verifiers_are_random_and_long_enough() {
        let a = random_urlsafe(VERIFIER_BYTES);
        assert_ne!(a, random_urlsafe(VERIFIER_BYTES));
        assert!(a.len() >= 43); // RFC 7636 minimum
    }
}
