//! The Google Calendar `Connector`: incremental read, and approved-only create and update.

use std::collections::BTreeMap;
use std::sync::Arc;

use haste_core::connector::{
    Batch, BoxFuture, CalendarSweep, Capabilities, Connector, ConnectorError, Cursor, DraftToSend,
    EventPatch, EventReceipt, NewEvent, SendReceipt,
};
use haste_core::model::{Event, Source};
use haste_google_oauth::auth::{OAuthClient, TokenSource};
use haste_google_oauth::keychain::TokenStore;
use serde::{Deserialize, Serialize};

use crate::client::{CalendarClient, EventsPage};
use crate::config::{ClientCredentials, GcalConfig};
use crate::consts::{API_BASE, HTTP_TIMEOUT, MAX_PAGES};
use crate::mapper::{insert_body, keep, map_event, patch_body};

/// Receives `(outcome, detail)` once per cycle. Details are counts only: never a title,
/// description, location or attendee address.
pub type CycleAudit = Arc<dyn Fn(&str, &str) + Send + Sync>;
/// Unix seconds; tests substitute a fixed clock.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Where each selected calendar's incremental read continues.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarCursor {
    /// Calendar id to its `nextSyncToken`.
    #[serde(default)]
    pub calendars: BTreeMap<String, String>,
}

impl CalendarCursor {
    #[must_use]
    pub fn decode(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or_default()
    }

    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

pub struct GcalConnector {
    client: CalendarClient,
    config: GcalConfig,
    audit: CycleAudit,
    clock: Clock,
}

enum Listed {
    Done {
        events: Vec<Event>,
        next_token: Option<String>,
    },
    Gone,
}

struct CalendarRead {
    events: Vec<Event>,
    token: Option<String>,
    full: bool,
    reset: bool,
}

impl GcalConnector {
    /// Production endpoints.
    #[must_use]
    pub fn from_config(
        config: &GcalConfig,
        credentials: &ClientCredentials,
        store: Arc<dyn TokenStore>,
        audit: CycleAudit,
    ) -> Self {
        Self::new(
            config,
            credentials,
            API_BASE,
            haste_google_oauth::TOKEN_URL,
            store,
            audit,
        )
    }

    /// Custom endpoints (tests point these at a local fake server).
    #[must_use]
    pub fn new(
        config: &GcalConfig,
        credentials: &ClientCredentials,
        api_base: &str,
        token_url: &str,
        store: Arc<dyn TokenStore>,
        audit: CycleAudit,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("http client");
        let tokens = TokenSource::new(
            http.clone(),
            token_url,
            OAuthClient {
                client_id: credentials.client_id.clone(),
                client_secret: credentials.client_secret.clone(),
            },
            &config.account,
            store,
        );
        Self {
            client: CalendarClient::new(http, api_base, Arc::new(tokens)),
            config: config.clone(),
            audit,
            clock: Arc::new(haste_core::time::now_secs),
        }
    }

    /// Replace the clock (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    async fn list_all(
        &self,
        calendar: &str,
        sync_token: Option<&str>,
        now: i64,
    ) -> Result<Listed, ConnectorError> {
        let mut events = Vec::new();
        let mut page: Option<String> = None;
        for _ in 0..MAX_PAGES {
            match self
                .client
                .list_events(calendar, sync_token, page.as_deref())
                .await?
            {
                EventsPage::Gone => return Ok(Listed::Gone),
                EventsPage::Page {
                    items,
                    next_page,
                    next_sync_token,
                } => {
                    for api in &items {
                        let event = map_event(calendar, api);
                        if keep(api, &event, now) {
                            events.push(event);
                        }
                    }
                    match next_page {
                        Some(p) => page = Some(p),
                        None => {
                            return Ok(Listed::Done {
                                events,
                                next_token: next_sync_token,
                            })
                        }
                    }
                }
            }
        }
        Err(ConnectorError::Other(
            "calendar has too many events for one cycle".into(),
        ))
    }

    /// Read one calendar: a delta with its token, or a full listing. When Google says the token
    /// is gone, relist once; the full listing itself is never retried.
    async fn read_calendar(
        &self,
        calendar: &str,
        mut token: Option<String>,
        now: i64,
    ) -> Result<CalendarRead, ConnectorError> {
        let mut reset = false;
        loop {
            let full = token.is_none();
            match self.list_all(calendar, token.as_deref(), now).await? {
                Listed::Gone if !full => {
                    token = None;
                    reset = true;
                }
                Listed::Gone => {
                    return Err(ConnectorError::Other("calendar listing unavailable".into()))
                }
                Listed::Done { events, next_token } => {
                    return Ok(CalendarRead {
                        events,
                        token: next_token,
                        full,
                        reset,
                    })
                }
            }
        }
    }

    async fn run_cycle(&self, cursor: Option<Cursor>) -> Result<Batch, ConnectorError> {
        let state = cursor
            .map(|c| CalendarCursor::decode(&c.0))
            .unwrap_or_default();
        let now = (self.clock)();
        let mut next = CalendarCursor::default();
        let mut batch = Batch::default();
        let (mut full_count, mut reset_count) = (0, 0);
        // Only calendars listed in gcal.json are ever requested.
        for calendar in &self.config.calendars {
            let read = match self
                .read_calendar(calendar, state.calendars.get(calendar).cloned(), now)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    (self.audit)(
                        match &e {
                            ConnectorError::Checkpoint(_) => "auth_or_setup",
                            ConnectorError::Challenge(_) => "challenge",
                            ConnectorError::Transient(_) => "transient",
                            _ => "error",
                        },
                        "",
                    );
                    return Err(e);
                }
            };
            if read.full {
                full_count += 1;
            }
            if read.reset {
                reset_count += 1;
            }
            if let Some(token) = read.token {
                if read.full {
                    batch.calendar_sweeps.push(CalendarSweep {
                        calendar: calendar.clone(),
                        id: format!("full-{now}"),
                    });
                }
                next.calendars.insert(calendar.clone(), token);
            }
            batch.events.extend(read.events);
        }
        (self.audit)(
            "ok",
            &format!(
                "calendars={} events={} full={full_count} resets={reset_count}",
                self.config.calendars.len(),
                batch.events.len()
            ),
        );
        batch.next_cursor = Some(Cursor(next.encode()));
        Ok(batch)
    }

    fn selected(&self, calendar: &str) -> Result<(), ConnectorError> {
        if self.config.selects(calendar) {
            Ok(())
        } else {
            // Nothing was sent: a refusal, not an outcome-unknown failure.
            Err(ConnectorError::Unsupported(
                "that calendar is not selected in gcal.json".into(),
            ))
        }
    }
}

impl Connector for GcalConnector {
    fn source(&self) -> Source {
        Source::GoogleCalendar
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            calendar_write: true,
            ..Default::default()
        }
    }

    fn default_engaged(&self) -> bool {
        true
    }

    fn fetch(&self, cursor: Option<Cursor>) -> BoxFuture<'_, Result<Batch, ConnectorError>> {
        Box::pin(self.run_cycle(cursor))
    }

    fn send<'a>(
        &'a self,
        _draft: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError::Unsupported(
                "the calendar connector cannot send".into(),
            ))
        })
    }

    fn create_event<'a>(
        &'a self,
        event: &'a NewEvent,
    ) -> BoxFuture<'a, Result<EventReceipt, ConnectorError>> {
        Box::pin(async move {
            self.selected(&event.calendar)?;
            let api = self
                .client
                .insert_event(&event.calendar, &insert_body(event))
                .await?;
            Ok(EventReceipt {
                event: map_event(&event.calendar, &api),
            })
        })
    }

    fn update_event<'a>(
        &'a self,
        external_id: &'a str,
        patch: &'a EventPatch,
    ) -> BoxFuture<'a, Result<EventReceipt, ConnectorError>> {
        Box::pin(async move {
            let (calendar, id) = external_id
                .split_once('/')
                .ok_or_else(|| ConnectorError::Unsupported("not a calendar event id".into()))?;
            self.selected(calendar)?;
            let body = patch_body(patch);
            if body.as_object().is_none_or(serde_json::Map::is_empty) {
                return Err(ConnectorError::Unsupported(
                    "the update changes nothing".into(),
                ));
            }
            let api = self.client.patch_event(calendar, id, &body).await?;
            Ok(EventReceipt {
                event: map_event(calendar, &api),
            })
        })
    }
}
