//! Thin Google Calendar REST client. Every failure becomes a `ConnectorError` through the shared
//! classifier. There is deliberately no delete method, and every write sends `sendUpdates=none`.

use std::sync::Arc;

use haste_core::connector::ConnectorError;
use haste_google_oauth::auth::TokenSource;
use haste_google_oauth::errors::classify;
use reqwest::Url;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::consts::PAGE_SIZE;

/// The only value of `sendUpdates` this client ever sends: Google emails nobody.
pub const SEND_UPDATES: &str = "none";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiCalendar {
    pub id: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub primary: bool,
}

#[derive(Debug, Default, Deserialize)]
struct CalendarList {
    #[serde(default)]
    items: Vec<ApiCalendar>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiTime {
    pub date: Option<String>,
    #[serde(rename = "dateTime")]
    pub date_time: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiAttendee {
    #[serde(default)]
    pub email: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiEvent {
    pub id: String,
    #[serde(default)]
    pub status: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: Option<ApiTime>,
    pub end: Option<ApiTime>,
    #[serde(default)]
    pub attendees: Vec<ApiAttendee>,
    pub updated: Option<String>,
    /// Present on recurring series (kept as one master event).
    #[serde(default)]
    pub recurrence: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventsList {
    #[serde(default)]
    items: Vec<ApiEvent>,
    next_page_token: Option<String>,
    next_sync_token: Option<String>,
}

/// One page of events, or the sign that the sync token expired.
#[derive(Debug)]
pub enum EventsPage {
    Page {
        items: Vec<ApiEvent>,
        next_page: Option<String>,
        next_sync_token: Option<String>,
    },
    /// HTTP 410: the sync token is gone; the caller relists once.
    Gone,
}

#[derive(Clone)]
pub struct CalendarClient {
    http: reqwest::Client,
    base: String,
    tokens: Arc<TokenSource>,
}

impl CalendarClient {
    #[must_use]
    pub fn new(http: reqwest::Client, base_url: &str, tokens: Arc<TokenSource>) -> Self {
        Self {
            http,
            base: base_url.trim_end_matches('/').to_owned(),
            tokens,
        }
    }

    /// Build `<base>/<segments...>` with each segment percent-encoded (calendar ids contain
    /// `@` and `#`).
    fn url(&self, segments: &[&str]) -> Result<Url, ConnectorError> {
        let mut url = Url::parse(&self.base)
            .map_err(|_| ConnectorError::Other("invalid calendar api base".into()))?;
        url.path_segments_mut()
            .map_err(|()| ConnectorError::Other("invalid calendar api base".into()))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(u16, String), ConnectorError> {
        let token = self.tokens.access_token().await?;
        let response = request.bearer_auth(token).send().await.map_err(|e| {
            ConnectorError::Transient(format!("calendar unreachable: {}", e.without_url()))
        })?;
        let status = response.status().as_u16();
        Ok((status, response.text().await.unwrap_or_default()))
    }

    fn parse<T: DeserializeOwned>(body: &str) -> Result<T, ConnectorError> {
        serde_json::from_str(body)
            .map_err(|_| ConnectorError::Other("malformed calendar response".into()))
    }

    /// The human's calendars.
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn list_calendars(&self) -> Result<Vec<ApiCalendar>, ConnectorError> {
        let url = self.url(&["users", "me", "calendarList"])?;
        let (status, body) = self.send(self.http.get(url)).await?;
        match status {
            200..=299 => Ok(Self::parse::<CalendarList>(&body)?.items),
            _ => Err(classify(status, &body)),
        }
    }

    /// One page of a calendar's events. With `sync_token` only changes since the token are
    /// returned (cancelled events included); without it the listing has no time window, which is
    /// what makes Google return a `nextSyncToken` on the last page.
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn list_events(
        &self,
        calendar: &str,
        sync_token: Option<&str>,
        page: Option<&str>,
    ) -> Result<EventsPage, ConnectorError> {
        let url = self.url(&["calendars", calendar, "events"])?;
        let mut query = vec![("maxResults", PAGE_SIZE.to_string())];
        if let Some(t) = sync_token {
            query.push(("syncToken", t.to_owned()));
        }
        if let Some(p) = page {
            query.push(("pageToken", p.to_owned()));
        }
        let (status, body) = self.send(self.http.get(url).query(&query)).await?;
        match status {
            410 => Ok(EventsPage::Gone),
            200..=299 => {
                let list: EventsList = Self::parse(&body)?;
                Ok(EventsPage::Page {
                    items: list.items,
                    next_page: list.next_page_token,
                    next_sync_token: list.next_sync_token,
                })
            }
            _ => Err(classify(status, &body)),
        }
    }

    /// Insert an event (`sendUpdates=none`).
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn insert_event(
        &self,
        calendar: &str,
        body: &serde_json::Value,
    ) -> Result<ApiEvent, ConnectorError> {
        let url = self.url(&["calendars", calendar, "events"])?;
        let request = self
            .http
            .post(url)
            .query(&[("sendUpdates", SEND_UPDATES)])
            .json(body);
        let (status, text) = self.send(request).await?;
        match status {
            200..=299 => Self::parse(&text),
            _ => Err(classify(status, &text)),
        }
    }

    /// Patch an event (`sendUpdates=none`).
    ///
    /// # Errors
    /// Classified [`ConnectorError`].
    pub async fn patch_event(
        &self,
        calendar: &str,
        event: &str,
        body: &serde_json::Value,
    ) -> Result<ApiEvent, ConnectorError> {
        let url = self.url(&["calendars", calendar, "events", event])?;
        let request = self
            .http
            .patch(url)
            .query(&[("sendUpdates", SEND_UPDATES)])
            .json(body);
        let (status, text) = self.send(request).await?;
        match status {
            200..=299 => Self::parse(&text),
            404 => Err(ConnectorError::Other("event not found".into())),
            _ => Err(classify(status, &text)),
        }
    }
}
