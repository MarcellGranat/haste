//! The Reminders `Connector`: polled reads through the helper, plus the two approved writes
//! (create, complete). Nothing here can delete or edit a reminder.

use std::sync::Arc;

use haste_core::connector::{
    Batch, BoxFuture, Capabilities, Connector, ConnectorError, Cursor, DraftToSend, NewReminder,
    ReminderReceipt, SendReceipt, Sweep,
};
use haste_core::model::Source;
use serde::{Deserialize, Serialize};

use crate::protocol::{RemindersRequest, RemindersResponse, FETCH_PAGE_SIZE};
use crate::runner::{Helper, HelperError};

/// Pages fetched per cycle; with [`FETCH_PAGE_SIZE`] this bounds one cycle.
pub const MAX_PAGES_PER_CYCLE: usize = 10;
/// A full sweep (the only thing that can mark a reminder missing) runs every this many cycles.
pub const SWEEP_EVERY_CYCLES: u32 = 20;

/// Receives `(outcome, detail)` once per cycle. Details are counts only: never a title, note
/// or list name.
pub type CycleAudit = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Where the next cycle starts. Stored by the daemon as an opaque string.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RemindersCursor {
    /// Highest `modified` seen so far; delta cycles ask for changes at or after it.
    #[serde(default)]
    pub since: i64,
    /// Cycles since the last complete sweep; `None` means a sweep has never completed.
    #[serde(default)]
    pub cycles_since_sweep: Option<u32>,
    /// Counter used to make each sweep id unique.
    #[serde(default)]
    pub sweeps_started: u64,
}

impl RemindersCursor {
    #[must_use]
    pub fn decode(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or_default()
    }

    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    fn sweep_due(&self) -> bool {
        self.cycles_since_sweep
            .is_none_or(|c| c >= SWEEP_EVERY_CYCLES)
    }
}

pub struct RemindersConnector {
    helper: Arc<dyn Helper>,
    audit: CycleAudit,
}

impl RemindersConnector {
    #[must_use]
    pub fn new(helper: Arc<dyn Helper>, audit: CycleAudit) -> Self {
        Self { helper, audit }
    }
}

fn helper_error(e: HelperError) -> ConnectorError {
    match e {
        HelperError::Missing(m) => {
            ConnectorError::Other(format!("{m}; build the helper (see the README)"))
        }
        HelperError::TimedOut => ConnectorError::Transient("reminders helper timed out".into()),
        HelperError::Exited(_) | HelperError::Malformed(_) => ConnectorError::Other(e.to_string()),
    }
}

/// Map a helper `error` reply to a connector error.
fn reply_error(kind: &str) -> ConnectorError {
    match kind {
        "not_authorized" => ConnectorError::Authorization(
            "permission needed: run `haste connector reminders auth` from a Terminal window".into(),
        ),
        other => ConnectorError::Other(format!("reminders helper refused: {other}")),
    }
}

fn unexpected() -> ConnectorError {
    ConnectorError::Other("reminders helper answered a different request".into())
}

/// One fetch cycle. Blocking: runs on a worker thread.
fn run_cycle(
    helper: &dyn Helper,
    audit: &CycleAudit,
    cursor: Option<Cursor>,
) -> Result<Batch, ConnectorError> {
    let mut state = cursor
        .map(|c| RemindersCursor::decode(&c.0))
        .unwrap_or_default();
    let sweeping = state.sweep_due();
    let since = if sweeping { 0 } else { state.since };
    let mut reminders = Vec::new();
    let mut offset = 0;
    let mut has_more = true;
    let mut pages = 0;
    let result = (|| {
        while has_more && pages < MAX_PAGES_PER_CYCLE {
            let reply = helper
                .call(&RemindersRequest::FetchChanged {
                    since,
                    offset,
                    limit: FETCH_PAGE_SIZE,
                })
                .map_err(helper_error)?;
            match reply {
                RemindersResponse::FetchChanged {
                    reminders: page,
                    has_more: more,
                } => {
                    offset += page.len();
                    reminders.extend(page);
                    has_more = more;
                    pages += 1;
                }
                RemindersResponse::Error { kind, .. } => return Err(reply_error(&kind)),
                _ => return Err(unexpected()),
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        let class = match &e {
            ConnectorError::Authorization(_) => "permission_needed",
            ConnectorError::Transient(_) => "transient",
            _ => "error",
        };
        audit(class, "");
        return Err(e);
    }

    // A sweep only counts when the whole set was read; a truncated one marks nothing missing
    // and is retried from the start next cycle.
    let complete = sweeping && !has_more;
    for r in &reminders {
        state.since = state.since.max(r.modified);
    }
    let sweep = if sweeping {
        state.sweeps_started += 1;
        Some(Sweep {
            id: format!("sweep-{}", state.sweeps_started),
            complete,
        })
    } else {
        None
    };
    if complete {
        state.cycles_since_sweep = Some(0);
    } else if let Some(c) = state.cycles_since_sweep.as_mut() {
        *c = c.saturating_add(1);
    }
    audit(
        "ok",
        &format!(
            "reminders={} pages={} sweep={}{}",
            reminders.len(),
            pages,
            if sweeping { "yes" } else { "no" },
            if sweeping && !complete {
                " (truncated)"
            } else {
                ""
            }
        ),
    );
    Ok(Batch {
        reminders: reminders
            .into_iter()
            .map(crate::protocol::WireReminder::into_reminder)
            .collect(),
        sweep,
        next_cursor: Some(Cursor(state.encode())),
        ..Batch::default()
    })
}

impl Connector for RemindersConnector {
    fn source(&self) -> Source {
        Source::Reminders
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            reminders_create: true,
            reminders_complete: true,
            ..Default::default()
        }
    }

    fn fetch(&self, cursor: Option<Cursor>) -> BoxFuture<'_, Result<Batch, ConnectorError>> {
        Box::pin(async move {
            let helper = Arc::clone(&self.helper);
            let audit = Arc::clone(&self.audit);
            tokio::task::spawn_blocking(move || run_cycle(helper.as_ref(), &audit, cursor))
                .await
                .unwrap_or_else(|_| Err(ConnectorError::Transient("sync task failed".into())))
        })
    }

    fn send<'a>(
        &'a self,
        _draft: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError::Unsupported(
                "reminders cannot send mail".into(),
            ))
        })
    }

    fn create_reminder<'a>(
        &'a self,
        reminder: &'a NewReminder,
    ) -> BoxFuture<'a, Result<ReminderReceipt, ConnectorError>> {
        Box::pin(async move {
            let helper = Arc::clone(&self.helper);
            let request = RemindersRequest::CreateReminder {
                title: reminder.title.clone(),
                due: reminder.due,
                list: reminder.list.clone(),
                notes: reminder.notes.clone(),
            };
            let reply = tokio::task::spawn_blocking(move || helper.call(&request))
                .await
                .map_err(|_| ConnectorError::Other("reminder task failed".into()))?
                .map_err(helper_error)?;
            match reply {
                RemindersResponse::CreateReminder { reminder } => Ok(ReminderReceipt {
                    external_id: reminder.external_id,
                }),
                RemindersResponse::Error { kind, .. } => Err(reply_error(&kind)),
                _ => Err(unexpected()),
            }
        })
    }

    fn complete_reminder<'a>(
        &'a self,
        external_id: &'a str,
    ) -> BoxFuture<'a, Result<(), ConnectorError>> {
        Box::pin(async move {
            let helper = Arc::clone(&self.helper);
            let request = RemindersRequest::CompleteReminder {
                external_id: external_id.to_owned(),
            };
            let reply = tokio::task::spawn_blocking(move || helper.call(&request))
                .await
                .map_err(|_| ConnectorError::Other("reminder task failed".into()))?
                .map_err(helper_error)?;
            match reply {
                RemindersResponse::CompleteReminder {} => Ok(()),
                RemindersResponse::Error { kind, .. } => Err(reply_error(&kind)),
                _ => Err(unexpected()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::WireReminder;
    use std::sync::Mutex;

    struct Fake {
        replies: Mutex<Vec<Result<RemindersResponse, HelperError>>>,
        requests: Mutex<Vec<RemindersRequest>>,
    }

    impl Fake {
        fn new(mut replies: Vec<Result<RemindersResponse, HelperError>>) -> Arc<Self> {
            replies.reverse();
            Arc::new(Self {
                replies: Mutex::new(replies),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    impl Helper for Fake {
        fn call(&self, request: &RemindersRequest) -> Result<RemindersResponse, HelperError> {
            self.requests.lock().unwrap().push(request.clone());
            self.replies
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(Err(HelperError::Exited(Some(1))))
        }
    }

    fn wire(id: &str, modified: i64) -> WireReminder {
        WireReminder {
            external_id: id.into(),
            title: "SECRET TITLE".into(),
            notes: "SECRET NOTES".into(),
            due: None,
            completed: false,
            list: "SECRET LIST".into(),
            modified,
        }
    }

    fn page(items: Vec<WireReminder>, has_more: bool) -> Result<RemindersResponse, HelperError> {
        Ok(RemindersResponse::FetchChanged {
            reminders: items,
            has_more,
        })
    }

    fn audited() -> (CycleAudit, Arc<Mutex<Vec<String>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        let audit: CycleAudit = Arc::new(move |o, d| sink.lock().unwrap().push(format!("{o}:{d}")));
        (audit, log)
    }

    #[test]
    fn the_first_cycle_is_a_complete_sweep_and_the_next_is_a_delta() {
        let (audit, log) = audited();
        let fake = Fake::new(vec![
            page(vec![wire("a", 10), wire("b", 20)], false),
            page(vec![wire("b", 30)], false),
        ]);
        let first = run_cycle(fake.as_ref(), &audit, None).unwrap();
        assert_eq!(first.sweep.as_ref().map(|s| s.complete), Some(true));
        assert_eq!(first.reminders.len(), 2);
        let cursor = RemindersCursor::decode(&first.next_cursor.clone().unwrap().0);
        assert_eq!((cursor.since, cursor.cycles_since_sweep), (20, Some(0)));
        let second = run_cycle(fake.as_ref(), &audit, first.next_cursor).unwrap();
        assert!(second.sweep.is_none(), "deltas never mark anything missing");
        let requests = fake.requests.lock().unwrap();
        assert!(matches!(
            requests[0],
            RemindersRequest::FetchChanged { since: 0, .. }
        ));
        assert!(matches!(
            requests[1],
            RemindersRequest::FetchChanged { since: 20, .. }
        ));
        drop(requests);
        let joined = log.lock().unwrap().join("|");
        assert!(joined.contains("ok:reminders=2 pages=1 sweep=yes"));
        assert!(!joined.contains("SECRET"), "audit carries counts only");
    }

    #[test]
    fn a_truncated_sweep_is_not_complete_and_repeats() {
        let (audit, _log) = audited();
        let pages: Vec<_> = (0..MAX_PAGES_PER_CYCLE)
            .map(|i| page(vec![wire(&format!("r{i}"), 1)], true))
            .collect();
        let fake = Fake::new(pages);
        let batch = run_cycle(fake.as_ref(), &audit, None).unwrap();
        assert_eq!(batch.sweep.as_ref().map(|s| s.complete), Some(false));
        let cursor = RemindersCursor::decode(&batch.next_cursor.unwrap().0);
        assert_eq!(
            cursor.cycles_since_sweep, None,
            "still needs a complete sweep"
        );
    }

    #[test]
    fn a_new_sweep_starts_after_the_interval() {
        let (audit, _log) = audited();
        let cursor = RemindersCursor {
            since: 5,
            cycles_since_sweep: Some(SWEEP_EVERY_CYCLES),
            sweeps_started: 1,
        };
        let fake = Fake::new(vec![page(vec![], false)]);
        let batch = run_cycle(fake.as_ref(), &audit, Some(Cursor(cursor.encode()))).unwrap();
        let sweep = batch.sweep.unwrap();
        assert!(sweep.complete);
        assert_eq!(sweep.id, "sweep-2", "each sweep has its own id");
    }

    #[test]
    fn missing_permission_is_an_authorization_error_not_a_breaker_error() {
        let (audit, log) = audited();
        let fake = Fake::new(vec![Ok(RemindersResponse::Error {
            kind: "not_authorized".into(),
            message: String::new(),
        })]);
        let err = run_cycle(fake.as_ref(), &audit, None).unwrap_err();
        assert!(matches!(err, ConnectorError::Authorization(_)));
        assert_eq!(log.lock().unwrap().as_slice(), ["permission_needed:"]);
    }

    #[test]
    fn a_timed_out_helper_is_transient_and_a_missing_one_is_a_plain_error() {
        let (audit, _log) = audited();
        let timed_out = Fake::new(vec![Err(HelperError::TimedOut)]);
        assert!(matches!(
            run_cycle(timed_out.as_ref(), &audit, None),
            Err(ConnectorError::Transient(_))
        ));
        let missing = Fake::new(vec![Err(HelperError::Missing("x".into()))]);
        assert!(matches!(
            run_cycle(missing.as_ref(), &audit, None),
            Err(ConnectorError::Other(_))
        ));
    }

    #[test]
    fn capabilities_allow_reading_and_the_two_reminder_writes_only() {
        let (audit, _log) = audited();
        let c = RemindersConnector::new(Fake::new(vec![]), audit);
        let caps = c.capabilities();
        assert!(caps.read && caps.reminders_create && caps.reminders_complete);
        assert!(!caps.draft && !caps.send && !caps.archive);
    }

    #[tokio::test]
    async fn create_and_complete_go_through_the_helper() {
        let (audit, _log) = audited();
        let fake = Fake::new(vec![
            Ok(RemindersResponse::CreateReminder {
                reminder: wire("new-id", 1),
            }),
            Ok(RemindersResponse::CompleteReminder {}),
            Ok(RemindersResponse::Error {
                kind: "already_completed".into(),
                message: String::new(),
            }),
        ]);
        let c = RemindersConnector::new(fake.clone(), audit);
        let receipt = c
            .create_reminder(&NewReminder {
                title: "t".into(),
                due: Some(5),
                list: None,
                notes: Some("haste:thread/x".into()),
            })
            .await
            .unwrap();
        assert_eq!(receipt.external_id, "new-id");
        c.complete_reminder("new-id").await.unwrap();
        assert!(c.complete_reminder("new-id").await.is_err());
        let requests = fake.requests.lock().unwrap();
        assert!(matches!(
            &requests[0],
            RemindersRequest::CreateReminder { notes: Some(n), .. } if n == "haste:thread/x"
        ));
    }
}
