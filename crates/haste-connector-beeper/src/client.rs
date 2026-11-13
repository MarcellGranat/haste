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
        let result = self.store.get(TOKEN_ACCOUNT);
        haste_core::log::event(
            if matches!(&result, Ok(Some(t)) if !t.is_empty()) {
                haste_core::log::Level::Debug
            } else {
                haste_core::log::Level::Error
            },
            "beeper",
            "keychain_read",
            &[haste_core::log::l(
                "result",
                match &result {
                    Ok(Some(t)) if !t.is_empty() => "ok",
                    Ok(_) => "absent",
                    Err(_) => "error",
                },
            )],
        );
        match result {
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

    /// A GET, logged with a route template (ids replaced by `:id`), the HTTP status, the error
    /// class and the duration. Never the URL, the query, the token or the body.
    async fn get<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, String)],
    ) -> Result<T, ConnectorError> {
        let timer = haste_core::log::Timer::start();
        let (status, result) = self.get_inner(segments, query).await;
        let route = route_of(segments);
        let (level, class) = match &result {
            Ok(_) => (haste_core::log::Level::Debug, ""),
            Err(e) => (
                haste_core::log::Level::Warn,
                haste_core::connector::error_class(e),
            ),
        };
        let mut fields = vec![
            haste_core::log::id("route", &route),
            haste_core::log::n("status", status),
            haste_core::log::n("ms", timer.ms()),
        ];
        if !class.is_empty() {
            fields.push(haste_core::log::id("error_class", class));
        }
        haste_core::log::event(level, "beeper", "http_get", &fields);
        result
    }

    async fn get_inner<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, String)],
    ) -> (i64, Result<T, ConnectorError>) {
        let token = match self.token() {
            Ok(t) => t,
            Err(e) => return (0, Err(e)),
        };
        let response = match self
            .http
            .get(self.url(segments))
            .query(query)
            .bearer_auth(token)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return (0, Err(Self::reach_error(&e))),
        };
        let status = response.status();
        let code = i64::from(status.as_u16());
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            self.forget();
            return (
                code,
                Err(ConnectorError::Authorization(TOKEN_REJECTED.into())),
            );
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return (
                code,
                Err(ConnectorError::Transient(format!(
                    "beeper http {}",
                    status.as_u16()
                ))),
            );
        }
        if !status.is_success() {
            return (
                code,
                Err(ConnectorError::Other(format!(
                    "beeper http {}",
                    status.as_u16()
                ))),
            );
        }
        let text = response.text().await.unwrap_or_default();
        (
            code,
            serde_json::from_str(&text)
                .map_err(|_| ConnectorError::Other("malformed beeper response".into())),
        )
    }

    /// The cheapest authenticated call: proves the token and that Beeper Desktop is running.
    /// `/v1/info` is NOT authenticated (spike 2026-11-10: it answers 200 to a wrong token), so the
    /// account list is the check.
    ///
    /// # Errors
    /// `Authorization` when Beeper is not running or the token is missing or rejected.
    pub async fn check(&self) -> Result<(), ConnectorError> {
        let _: serde_json::Value = self.get(&["v1", "accounts"], &[]).await?;
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

    /// Chats whose name matches `query` (Beeper's own search, first page).
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn search_chats(&self, query: &str) -> Result<Vec<WireChat>, ConnectorError> {
        let list: List<WireChat> = self
            .get(&["v1", "chats", "search"], &[("query", query.to_owned())])
            .await?;
        Ok(list.into_parts().0)
    }

    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn chat(&self, chat_id: &str) -> Result<WireChat, ConnectorError> {
        self.get(&["v1", "chats", chat_id], &[]).await
    }

    /// A chat by id, or `None` when Beeper does not know it (a stale selection is skipped, not a
    /// failure of the whole cycle).
    ///
    /// # Errors
    /// Classified [`ConnectorError`] for anything but a missing chat.
    pub async fn chat_if_exists(&self, chat_id: &str) -> Result<Option<WireChat>, ConnectorError> {
        match self.chat(chat_id).await {
            Ok(chat) => Ok(Some(chat)),
            Err(ConnectorError::Other(m)) if m == "beeper http 404" => Ok(None),
            Err(e) => Err(e),
        }
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
        self.send_message_with(chat_id, text, None).await
    }

    /// Upload a file to the local Beeper service so a message can carry it. Nothing is sent to
    /// anyone by this call, so a failure is a plain refusal (never an unknown outcome).
    ///
    /// # Errors
    /// `Authorization` when Beeper is not running or the token is rejected, else `Other`.
    pub async fn upload_asset(
        &self,
        name: &str,
        mime: &str,
        data: &[u8],
    ) -> Result<String, ConnectorError> {
        use base64::Engine;
        let token = self.token()?;
        let response = self
            .http
            .post(self.url(&["v1", "assets", "upload", "base64"]))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "content": base64::engine::general_purpose::STANDARD.encode(data),
                "fileName": name,
                "mimeType": mime,
            }))
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    ConnectorError::Authorization(NOT_RUNNING.into())
                } else {
                    ConnectorError::Other("upload failed".into())
                }
            })?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            self.forget();
            return Err(ConnectorError::Authorization(TOKEN_REJECTED.into()));
        }
        if !status.is_success() {
            return Err(ConnectorError::Other(format!(
                "upload refused: http {}",
                status.as_u16()
            )));
        }
        let text = response.text().await.unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("uploadID")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ConnectorError::Other("upload gave no id".into()))
    }

    /// Like [`Self::send_message`], with one uploaded file (`(upload id, file name)`).
    ///
    /// # Errors
    /// The same classes as [`Self::send_message`].
    pub async fn send_message_with(
        &self,
        chat_id: &str,
        text: &str,
        attachment: Option<(&str, &str)>,
    ) -> Result<String, ConnectorError> {
        let token = self.token()?;
        let timer = haste_core::log::Timer::start();
        let mut payload = serde_json::Map::new();
        if !text.is_empty() {
            payload.insert("text".into(), serde_json::Value::String(text.to_owned()));
        }
        if let Some((upload_id, name)) = attachment {
            payload.insert(
                "attachment".into(),
                serde_json::json!({ "uploadID": upload_id, "fileName": name }),
            );
        }
        let response = self
            .http
            .post(self.url(&["v1", "chats", chat_id, "messages"]))
            .bearer_auth(token)
            .json(&serde_json::Value::Object(payload))
            .send()
            .await
            .map_err(|e| {
                // which kind of transport failure it was, and how long it took: this is what tells
                // "never reached Beeper" from "Beeper took too long"
                let kind = if e.is_connect() {
                    "connect"
                } else if e.is_timeout() {
                    "timeout"
                } else if e.is_request() {
                    "request"
                } else if e.is_body() {
                    "body"
                } else {
                    "other"
                };
                haste_core::log::error(
                    "beeper",
                    "send_transport_error",
                    &[
                        haste_core::log::l("kind", kind),
                        haste_core::log::n("ms", timer.ms()),
                        haste_core::log::n("body_bytes", text.len()),
                    ],
                );
                if e.is_connect() {
                    ConnectorError::Authorization(NOT_RUNNING.into())
                } else {
                    ConnectorError::Ambiguous("send outcome unknown".into())
                }
            })?;
        let status = response.status();
        haste_core::log::info(
            "beeper",
            "send_response",
            &[
                haste_core::log::n("status", status.as_u16()),
                haste_core::log::n("ms", timer.ms()),
                haste_core::log::n("body_bytes", text.len()),
            ],
        );
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

/// `v1/chats/:id/messages` from `["v1", "chats", <id>, "messages"]`: the chat id is never logged.
fn route_of(segments: &[&str]) -> String {
    segments
        .iter()
        .enumerate()
        .map(|(i, s)| if i == 2 && *s != "search" { ":id" } else { *s })
        .collect::<Vec<_>>()
        .join("/")
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
