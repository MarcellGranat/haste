//! The HTTP client for Beeper Desktop's local API: loopback only, no redirects, no retry, and the
//! token only ever from the token store (the Keychain). Errors carry fixed labels: never the
//! token, a chat name, message text or an account id.

use std::sync::{Arc, Mutex};

use haste_core::connector::ConnectorError;
use haste_google_oauth::keychain::TokenStore;
use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;

use crate::config::validate_base_url;
use crate::consts::{BEEPER_REQUEST_TIMEOUT, KEYCHAIN_SERVICE, TOKEN_ACCOUNT};
use crate::wire::{List, WireAccount, WireChat, WireMessage, WireSent};

const NOT_RUNNING: &str = "Beeper Desktop is not running";
const TOKEN_REJECTED: &str = "Beeper token rejected; run `haste connector beeper auth`";
const NO_TOKEN: &str = "no Beeper token stored; run `haste connector beeper auth`";

pub struct BeeperClient {
    http: Client,
    base: String,
    store: Arc<dyn TokenStore>,
    /// The token, read from the store on first use and kept in memory only.
    cached: Mutex<Option<String>>,
}

impl BeeperClient {
    /// # Errors
    /// A fixed reason when `base` is not a loopback `http` URL.
    pub fn new(base: &str, store: Arc<dyn TokenStore>) -> Result<Self, &'static str> {
        validate_base_url(base)?;
        let http = Client::builder()
            .timeout(BEEPER_REQUEST_TIMEOUT)
            // a redirect could carry the token somewhere else: never follow one
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "http client")?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            store,
            cached: Mutex::new(None),
        })
    }

    fn token(&self) -> Result<String, ConnectorError> {
        if let Some(t) = self.cached.lock().ok().and_then(|c| c.clone()) {
            return Ok(t);
        }
        match self.store.get(TOKEN_ACCOUNT) {
            Ok(Some(t)) if !t.is_empty() => {
                if let Ok(mut c) = self.cached.lock() {
                    *c = Some(t.clone());
                }
                Ok(t)
            }
            Ok(_) => Err(ConnectorError::Authorization(NO_TOKEN.into())),
            Err(_) => Err(ConnectorError::Authorization(format!(
                "the Keychain item {KEYCHAIN_SERVICE} could not be read"
            ))),
        }
    }

    /// Forget the cached token (after it was rejected).
    fn forget(&self) {
        if let Ok(mut c) = self.cached.lock() {
            *c = None;
        }
    }

    fn url(&self, segments: &[&str]) -> String {
        let mut url = self.base.clone();
        for s in segments {
            url.push('/');
            url.push_str(&encode(s));
        }
        url
    }

    /// Connection refused and friends: Beeper Desktop is not running (a state waiting on the
    /// human, never a breaker event).
    fn reach_error(e: &reqwest::Error) -> ConnectorError {
        if e.is_connect() {
            ConnectorError::Authorization(NOT_RUNNING.into())
        } else {
            ConnectorError::Transient("beeper request failed".into())
        }
    }

    async fn get<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, String)],
    ) -> Result<T, ConnectorError> {
        let token = self.token()?;
        let response = self
            .http
            .get(self.url(segments))
            .query(query)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| Self::reach_error(&e))?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            self.forget();
            return Err(ConnectorError::Authorization(TOKEN_REJECTED.into()));
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(ConnectorError::Transient(format!(
                "beeper http {}",
                status.as_u16()
            )));
        }
        if !status.is_success() {
            return Err(ConnectorError::Other(format!(
                "beeper http {}",
                status.as_u16()
            )));
        }
        let text = response.text().await.unwrap_or_default();
        serde_json::from_str(&text)
            .map_err(|_| ConnectorError::Other("malformed beeper response".into()))
    }

    /// The cheapest authenticated call: proves the token and that Beeper Desktop is running.
    ///
    /// # Errors
    /// `Authorization` when Beeper is not running or the token is missing or rejected.
    pub async fn check(&self) -> Result<(), ConnectorError> {
        let _: serde_json::Value = self.get(&["v1", "info"], &[]).await?;
        Ok(())
    }

    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn accounts(&self) -> Result<Vec<WireAccount>, ConnectorError> {
        let list: List<WireAccount> = self.get(&["v1", "accounts"], &[]).await?;
        Ok(list.into_parts().0)
    }

    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn chats(&self) -> Result<Vec<WireChat>, ConnectorError> {
        let list: List<WireChat> = self.get(&["v1", "chats"], &[]).await?;
        Ok(list.into_parts().0)
    }

    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn chat(&self, chat_id: &str) -> Result<WireChat, ConnectorError> {
        self.get(&["v1", "chats", chat_id], &[]).await
    }

    /// One page of a chat's messages (newest first, as far as the API says; the caller does not
    /// rely on the order). Returns the items, whether more exist and the cursor for the next page.
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn messages(
        &self,
        chat_id: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Vec<WireMessage>, bool, Option<String>), ConnectorError> {
        let mut query = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_owned()));
        }
        let list: List<WireMessage> = self
            .get(&["v1", "chats", chat_id, "messages"], &query)
            .await?;
        Ok(list.into_parts())
    }

    /// Send one text message into an existing chat: exactly one request, never retried. The
    /// answer is classified for the at-most-once rule: refusals that delivered nothing are
    /// `Authorization`/`Other`, a rate-limit or restriction is `Checkpoint`, and anything that
    /// may have reached the network (a 5xx, a timeout, an answer without a message id) is
    /// `Ambiguous`.
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn send_message(&self, chat_id: &str, text: &str) -> Result<String, ConnectorError> {
        let token = self.token()?;
        let response = self
            .http
            .post(self.url(&["v1", "chats", chat_id, "messages"]))
            .bearer_auth(token)
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    ConnectorError::Authorization(NOT_RUNNING.into())
                } else {
                    ConnectorError::Ambiguous("send outcome unknown".into())
                }
            })?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            self.forget();
            return Err(ConnectorError::Authorization(TOKEN_REJECTED.into()));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(ConnectorError::Checkpoint(
                "Beeper reports rate limiting; the source is stopped until you release it".into(),
            ));
        }
        if status.is_client_error() || status.is_redirection() {
            return Err(ConnectorError::Other(format!(
                "send refused: http {}",
                status.as_u16()
            )));
        }
        if !status.is_success() {
            return Err(ConnectorError::Ambiguous(format!(
                "http {}",
                status.as_u16()
            )));
        }
        let text = response.text().await.unwrap_or_default();
        serde_json::from_str::<WireSent>(&text)
            .ok()
            .and_then(|s| s.id)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ConnectorError::Ambiguous("send answer had no message id".into()))
    }
}

/// Percent-encode a path segment (chat ids contain characters like `!`, `:` and `@`).
fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(b).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use haste_google_oauth::keychain::MemoryStore;

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(encode("!abc:beeper.local"), "%21abc%3Abeeper.local");
        assert_eq!(encode("a/b"), "a%2Fb");
        assert_eq!(encode("plain-id_1.x~"), "plain-id_1.x~");
    }

    #[test]
    fn a_non_loopback_base_url_is_refused_before_any_connection() {
        let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
        for bad in [
            "https://127.0.0.1:23373",
            "http://example.com",
            "http://127.0.0.1.evil.com",
        ] {
            assert!(BeeperClient::new(bad, Arc::clone(&store)).is_err(), "{bad}");
        }
        assert!(BeeperClient::new("http://127.0.0.1:23373", store).is_ok());
    }
}
