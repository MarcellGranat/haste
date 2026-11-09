//! Thin Gmail REST client. Every failure is mapped to a `ConnectorError`.

use std::sync::Arc;

use haste_core::connector::ConnectorError;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::auth::TokenSource;
use crate::errors::classify;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub email_address: String,
    pub history_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThreadRef {
    pub id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadList {
    #[serde(default)]
    pub threads: Vec<ThreadRef>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct HistoryMessage {
    #[serde(rename = "threadId", default)]
    pub thread_id: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct HistoryRecord {
    #[serde(default)]
    pub messages: Vec<HistoryMessage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryList {
    #[serde(default)]
    pub history: Vec<HistoryRecord>,
    pub history_id: Option<String>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DraftCreated {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiBody {
    pub data: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiPart {
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub headers: Vec<ApiHeader>,
    #[serde(default)]
    pub body: ApiBody,
    #[serde(default)]
    pub parts: Vec<ApiPart>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiMessage {
    pub id: String,
    #[serde(default)]
    pub thread_id: String,
    #[serde(default)]
    pub label_ids: Vec<String>,
    #[serde(default)]
    pub snippet: String,
    pub internal_date: Option<String>,
    pub payload: Option<ApiPart>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiThread {
    pub id: String,
    #[serde(default)]
    pub messages: Vec<ApiMessage>,
}

#[derive(Clone)]
pub struct GmailClient {
    http: reqwest::Client,
    base: String,
    tokens: Arc<TokenSource>,
}

type Query<'a> = &'a [(&'a str, String)];

impl GmailClient {
    #[must_use]
    pub fn new(http: reqwest::Client, base_url: &str, tokens: Arc<TokenSource>) -> Self {
        Self {
            http,
            base: base_url.trim_end_matches('/').to_owned(),
            tokens,
        }
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<Option<String>, ConnectorError> {
        let token = self.tokens.access_token().await?;
        let response = request.bearer_auth(token).send().await.map_err(|e| {
            ConnectorError::Transient(format!("gmail unreachable: {}", e.without_url()))
        })?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        match status {
            200..=299 => Ok(Some(body)),
            404 => Ok(None),
            _ => Err(classify(status, &body)),
        }
    }

    fn parse<T: DeserializeOwned>(body: &str) -> Result<T, ConnectorError> {
        serde_json::from_str(body)
            .map_err(|_| ConnectorError::Other("malformed gmail response".into()))
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: Query<'_>,
    ) -> Result<Option<T>, ConnectorError> {
        let request = self.http.get(format!("{}{path}", self.base)).query(query);
        match self.send(request).await? {
            Some(body) => Self::parse(&body).map(Some),
            None => Ok(None),
        }
    }

    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn profile(&self) -> Result<Profile, ConnectorError> {
        self.get("/gmail/v1/users/me/profile", &[])
            .await?
            .ok_or_else(|| ConnectorError::Other("profile not found".into()))
    }

    /// One page of threads newer than `days` days.
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn list_threads(
        &self,
        days: u32,
        page: Option<&str>,
    ) -> Result<ThreadList, ConnectorError> {
        let mut query = vec![
            ("q", format!("newer_than:{days}d")),
            ("maxResults", "100".to_owned()),
        ];
        if let Some(p) = page {
            query.push(("pageToken", p.to_owned()));
        }
        Ok(self
            .get("/gmail/v1/users/me/threads", &query)
            .await?
            .unwrap_or_default())
    }

    /// Full thread, or `None` when it no longer exists.
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn get_thread(&self, id: &str) -> Result<Option<ApiThread>, ConnectorError> {
        self.get(
            &format!("/gmail/v1/users/me/threads/{id}"),
            &[("format", "full".to_owned())],
        )
        .await
    }

    /// Headers only; used to thread a reply.
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn get_thread_headers(&self, id: &str) -> Result<Option<ApiThread>, ConnectorError> {
        let mut query = vec![("format", "metadata".to_owned())];
        for h in ["Subject", "Message-ID", "References", "Reply-To", "From"] {
            query.push(("metadataHeaders", h.to_owned()));
        }
        self.get(&format!("/gmail/v1/users/me/threads/{id}"), &query)
            .await
    }

    /// History since `start`; `None` when Gmail no longer has that history (HTTP 404).
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn list_history(
        &self,
        start: &str,
        page: Option<&str>,
    ) -> Result<Option<HistoryList>, ConnectorError> {
        let mut query = vec![
            ("startHistoryId", start.to_owned()),
            ("maxResults", "500".to_owned()),
        ];
        if let Some(p) = page {
            query.push(("pageToken", p.to_owned()));
        }
        self.get("/gmail/v1/users/me/history", &query).await
    }

    /// Create a draft from a base64url MIME message, optionally inside a thread.
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn create_draft(
        &self,
        raw: &str,
        thread_id: Option<&str>,
    ) -> Result<DraftCreated, ConnectorError> {
        let mut message = serde_json::json!({ "raw": raw });
        if let Some(t) = thread_id {
            message["threadId"] = serde_json::Value::String(t.to_owned());
        }
        let request = self
            .http
            .post(format!("{}/gmail/v1/users/me/drafts", self.base))
            .json(&serde_json::json!({ "message": message }));
        match self.send(request).await? {
            Some(body) => Self::parse(&body),
            None => Err(ConnectorError::Other("draft endpoint not found".into())),
        }
    }
}
