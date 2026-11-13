//! Connector trait plus the guard that enforces capabilities, kill switch and circuit breaker.
//!
//! Async approach (task 8.1): methods return boxed futures so the trait stays object-safe
//! and no extra dependency is needed.

use std::future::Future;
use std::pin::Pin;

use crate::model::{Event, Message, Reminder, Source, Thread};

/// Attempts allowed for transient errors within one scheduled cycle.
pub const TRANSIENT_ATTEMPTS_PER_CYCLE: u32 = 3;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub read: bool,
    pub draft: bool,
    pub send: bool,
    pub archive: bool,
    /// May create a reminder (only after a human approved the proposal).
    pub reminders_create: bool,
    /// May mark a reminder completed (only after a human approved the proposal).
    pub reminders_complete: bool,
    /// May create or update calendar events (only after a human approved the proposal).
    pub calendar_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor(pub String);

/// Tells the store how a batch of reminders relates to a full sweep, so that a reminder is only
/// ever marked missing after a complete, untruncated sweep did not return it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sweep {
    /// Identifies one sweep; every reminder returned in it is stamped with this id.
    pub id: String,
    /// This batch ends the sweep (the last page, not truncated).
    pub complete: bool,
}

/// A complete, untruncated listing of one calendar: events of that calendar not stamped with
/// `id` are marked missing. Scoped per calendar so a reset of one calendar never touches another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarSweep {
    pub calendar: String,
    pub id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Batch {
    pub threads: Vec<Thread>,
    pub messages: Vec<Message>,
    /// Reminders (only the Reminders connector fills this).
    pub reminders: Vec<Reminder>,
    pub sweep: Option<Sweep>,
    /// Calendar events (only the Calendar connector fills these).
    pub events: Vec<Event>,
    pub calendar_sweeps: Vec<CalendarSweep>,
    pub next_cursor: Option<Cursor>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftToSend {
    pub thread_id: Option<String>,
    pub to: Option<String>,
    pub body: String,
    /// The resolved, validated recipients of a human send (empty for creating a draft).
    pub recipients: Vec<String>,
    /// The subject of a human send, already `Re: …` and validated.
    pub subject: Option<String>,
    /// The provider draft made at approval time, removed after a successful send (Gmail).
    pub approval_draft_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReceipt {
    pub external_id: String,
}

/// Provider-side draft that now exists (never sent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftReceipt {
    pub draft_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorError {
    /// The provider asked us to prove we are human. Never retried.
    Challenge(String),
    /// The provider placed a checkpoint/restriction on the account. Never retried.
    Checkpoint(String),
    Transient(String),
    Other(String),
    /// The connector does not support this call (the default for optional trait methods).
    Unsupported(String),
    /// A send that may or may not have reached the recipient (timeout or dropped connection
    /// after the message was handed over). Never retried: a human checks.
    Ambiguous(String),
    /// Access is not granted (for example macOS Reminders permission). Waiting on a human: it is
    /// never retried and never trips the circuit breaker.
    Authorization(String),
}

/// A reminder to create. `notes` carries only a haste identifier (see the reminder-links spec).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReminder {
    pub title: String,
    pub due: Option<i64>,
    pub list: Option<String>,
    pub notes: Option<String>,
}

/// A human's change to a reminder: the title and due date are set, text is only ever appended to
/// the notes (the old notes are kept).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderPatch {
    pub title: String,
    pub due: Option<i64>,
    pub append_notes: Option<String>,
}

/// A calendar event to create. Attendees are stored on the event but never emailed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub calendar: String,
    pub title: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub attendees: Vec<String>,
    pub description: Option<String>,
}

/// The only fields an update may carry: no attendees, no status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventPatch {
    pub title: Option<String>,
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub description: Option<String>,
    pub location: Option<String>,
}

/// Provider-side result of creating or updating an event: the event as it now exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventReceipt {
    pub event: Event,
}

/// Provider-side result of creating a reminder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderReceipt {
    pub external_id: String,
}

pub trait Connector: Send + Sync {
    fn source(&self) -> Source;
    fn capabilities(&self) -> Capabilities;
    fn fetch(&self, cursor: Option<Cursor>) -> BoxFuture<'_, Result<Batch, ConnectorError>>;
    fn send<'a>(
        &'a self,
        draft: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>>;

    /// Create a draft at the provider without sending it. `draft.thread_id` carries the
    /// provider's own thread id (the thread's `external_id`). Connectors without a `draft`
    /// capability keep this default.
    fn create_draft<'a>(
        &'a self,
        _draft: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<DraftReceipt, ConnectorError>> {
        Box::pin(async { Err(ConnectorError::Other("drafts are not supported".into())) })
    }

    /// Create a reminder. Default: unsupported. Only called after a human approved the proposal.
    fn create_reminder<'a>(
        &'a self,
        _reminder: &'a NewReminder,
    ) -> BoxFuture<'a, Result<ReminderReceipt, ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError::Unsupported(
                "reminders cannot be created".into(),
            ))
        })
    }

    /// The connector's own mail address, so a reply is never addressed to oneself.
    fn own_address(&self) -> Option<String> {
        None
    }

    /// Whether the kill switch starts engaged when no state is persisted yet (fresh install).
    fn default_engaged(&self) -> bool {
        false
    }

    /// Create a calendar event. Default: unsupported. Only called after a human approved.
    fn create_event<'a>(
        &'a self,
        _event: &'a NewEvent,
    ) -> BoxFuture<'a, Result<EventReceipt, ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError::Unsupported(
                "events cannot be created".into(),
            ))
        })
    }

    /// Change an event (title, times, description, location). Default: unsupported.
    fn update_event<'a>(
        &'a self,
        _external_id: &'a str,
        _patch: &'a EventPatch,
    ) -> BoxFuture<'a, Result<EventReceipt, ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError::Unsupported(
                "events cannot be updated".into(),
            ))
        })
    }

    /// Change a reminder's title and due date and append to its notes. Default: unsupported.
    /// Only the human's own TUI edit calls it.
    fn update_reminder<'a>(
        &'a self,
        _external_id: &'a str,
        _patch: &'a ReminderPatch,
    ) -> BoxFuture<'a, Result<(), ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError::Unsupported(
                "reminders cannot be edited".into(),
            ))
        })
    }

    /// Mark a reminder completed. Default: unsupported. Only called after a human approved it.
    fn complete_reminder<'a>(
        &'a self,
        _external_id: &'a str,
    ) -> BoxFuture<'a, Result<(), ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError::Unsupported(
                "reminders cannot be completed".into(),
            ))
        })
    }
}

/// Everything the guard needs from the daemon: persistence, audit and notifications.
pub trait Hooks: Send + Sync {
    fn kill_switch_engaged(&self, source: &Source) -> bool;
    fn set_kill_switch(&self, source: &Source, engaged: bool);
    fn audit(&self, action: &str, source: &Source, outcome: &str);
    fn notify(&self, message: &str);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    CapabilityMissing(&'static str),
    KillSwitchEngaged,
    /// The breaker tripped on this call; the switch is now engaged.
    BreakerOpened(ConnectorError),
    /// Transient failures used the whole per-cycle budget.
    TransientExhausted(ConnectorError),
    Connector(ConnectorError),
}

/// A fixed class for an error, for the operational log (never the error text).
#[must_use]
pub fn error_class(e: &ConnectorError) -> &'static str {
    match e {
        ConnectorError::Authorization(_) => "authorization",
        ConnectorError::Transient(_) => "transient",
        ConnectorError::Challenge(_) => "challenge",
        ConnectorError::Checkpoint(_) => "checkpoint",
        ConnectorError::Ambiguous(_) => "ambiguous",
        ConnectorError::Unsupported(_) => "unsupported",
        ConnectorError::Other(_) => "other",
    }
}

fn log_call(source: &Source, call: &'static str, outcome: &'static str, ms: i64, class: &str) {
    let level = match outcome {
        "ok" => crate::log::Level::Info,
        _ => crate::log::Level::Warn,
    };
    let mut fields = vec![
        crate::log::id("source", &source.as_name()),
        crate::log::l("call", call),
        crate::log::l("outcome", outcome),
        crate::log::n("ms", ms),
    ];
    if !class.is_empty() {
        fields.push(crate::log::id("error_class", class));
    }
    crate::log::event(level, "connector", "call", &fields);
}

/// Wraps a connector so every call is gated, audited and bounded.
pub struct Guarded<'a> {
    pub connector: &'a dyn Connector,
    pub hooks: &'a dyn Hooks,
}

impl Guarded<'_> {
    fn precheck(&self, flag: bool, name: &'static str) -> Result<(), CallError> {
        let source = self.connector.source();
        if self.hooks.kill_switch_engaged(&source) {
            self.hooks.audit(name, &source, "refused_kill_switch");
            log_call(&source, name, "refused_kill_switch", 0, "");
            return Err(CallError::KillSwitchEngaged);
        }
        if !flag {
            self.hooks.audit(name, &source, "refused_capability");
            log_call(&source, name, "refused_capability", 0, "");
            return Err(CallError::CapabilityMissing(name));
        }
        Ok(())
    }

    fn trip(&self, name: &str, error: ConnectorError) -> CallError {
        let source = self.connector.source();
        self.hooks.set_kill_switch(&source, true);
        self.hooks.audit(name, &source, "breaker_opened");
        crate::log::event(
            crate::log::Level::Error,
            "connector",
            "breaker_opened",
            &[
                crate::log::id("source", &source.as_name()),
                crate::log::id("call", name),
                crate::log::id("error_class", error_class(&error)),
            ],
        );
        self.hooks.notify(&format!(
            "connector {} stopped by circuit breaker; release the kill switch after checking the account",
            source.as_name()
        ));
        CallError::BreakerOpened(error)
    }

    /// Fetch with capability check, kill switch, breaker and bounded transient retries.
    ///
    /// # Errors
    /// See [`CallError`].
    pub async fn fetch(&self, cursor: Option<Cursor>) -> Result<Batch, CallError> {
        self.precheck(self.connector.capabilities().read, "fetch")?;
        let source = self.connector.source();
        let mut last = ConnectorError::Other("no attempt made".into());
        for attempt in 0..TRANSIENT_ATTEMPTS_PER_CYCLE {
            let timer = crate::log::Timer::start();
            match self.connector.fetch(cursor.clone()).await {
                Ok(batch) => {
                    self.hooks.audit("fetch", &source, "ok");
                    crate::log::info(
                        "connector",
                        "fetch_ok",
                        &[
                            crate::log::id("source", &source.as_name()),
                            crate::log::n("ms", timer.ms()),
                            crate::log::n("attempt", attempt + 1),
                            crate::log::n("threads", batch.threads.len()),
                            crate::log::n("messages", batch.messages.len()),
                        ],
                    );
                    return Ok(batch);
                }
                Err(e @ (ConnectorError::Challenge(_) | ConnectorError::Checkpoint(_))) => {
                    return Err(self.trip("fetch", e));
                }
                Err(e @ ConnectorError::Transient(_)) => {
                    self.hooks.audit("fetch", &source, "transient_error");
                    log_call(&source, "fetch", "transient_error", timer.ms(), "transient");
                    last = e;
                }
                // Waiting on a human: no retry, no breaker.
                Err(e @ ConnectorError::Authorization(_)) => {
                    self.hooks.audit("fetch", &source, "permission_needed");
                    log_call(
                        &source,
                        "fetch",
                        "permission_needed",
                        timer.ms(),
                        "authorization",
                    );
                    return Err(CallError::Connector(e));
                }
                Err(e) => {
                    self.hooks.audit("fetch", &source, "error");
                    log_call(&source, "fetch", "error", timer.ms(), error_class(&e));
                    return Err(CallError::Connector(e));
                }
            }
        }
        Err(CallError::TransientExhausted(last))
    }

    /// Create a provider draft after a human approved the proposal. Never retried; a
    /// challenge or checkpoint trips the breaker like any other call.
    ///
    /// # Errors
    /// See [`CallError`].
    pub async fn create_draft(&self, draft: &DraftToSend) -> Result<DraftReceipt, CallError> {
        self.precheck(self.connector.capabilities().draft, "draft.create")?;
        let source = self.connector.source();
        match self.connector.create_draft(draft).await {
            Ok(receipt) => {
                self.hooks.audit("draft.create", &source, "ok");
                Ok(receipt)
            }
            Err(e @ (ConnectorError::Challenge(_) | ConnectorError::Checkpoint(_))) => {
                Err(self.trip("draft.create", e))
            }
            Err(e) => {
                self.hooks.audit("draft.create", &source, "error");
                Err(CallError::Connector(e))
            }
        }
    }

    /// Create a reminder after a human approved the proposal. Never retried. An authorization
    /// problem is reported as such: it does not trip the circuit breaker (retrying cannot help
    /// until a human grants access).
    ///
    /// # Errors
    /// See [`CallError`].
    pub async fn create_reminder(
        &self,
        reminder: &NewReminder,
    ) -> Result<ReminderReceipt, CallError> {
        self.precheck(
            self.connector.capabilities().reminders_create,
            "reminder.create",
        )?;
        let source = self.connector.source();
        match self.connector.create_reminder(reminder).await {
            Ok(receipt) => {
                self.hooks.audit("reminder.create", &source, "ok");
                Ok(receipt)
            }
            Err(e @ (ConnectorError::Challenge(_) | ConnectorError::Checkpoint(_))) => {
                Err(self.trip("reminder.create", e))
            }
            Err(e) => {
                self.hooks.audit("reminder.create", &source, "error");
                Err(CallError::Connector(e))
            }
        }
    }

    /// Apply the human's edit to a reminder. Never retried.
    ///
    /// # Errors
    /// See [`CallError`].
    pub async fn update_reminder(
        &self,
        external_id: &str,
        patch: &ReminderPatch,
    ) -> Result<(), CallError> {
        self.precheck(
            self.connector.capabilities().reminders_create,
            "reminder.update",
        )?;
        let source = self.connector.source();
        match self.connector.update_reminder(external_id, patch).await {
            Ok(()) => {
                self.hooks.audit("reminder.update", &source, "ok");
                Ok(())
            }
            Err(e @ (ConnectorError::Challenge(_) | ConnectorError::Checkpoint(_))) => {
                Err(self.trip("reminder.update", e))
            }
            Err(e) => {
                self.hooks.audit("reminder.update", &source, "error");
                Err(CallError::Connector(e))
            }
        }
    }

    /// Mark a reminder completed after a human approved the proposal. Never retried.
    ///
    /// # Errors
    /// See [`CallError`].
    pub async fn complete_reminder(&self, external_id: &str) -> Result<(), CallError> {
        self.precheck(
            self.connector.capabilities().reminders_complete,
            "reminder.complete",
        )?;
        let source = self.connector.source();
        match self.connector.complete_reminder(external_id).await {
            Ok(()) => {
                self.hooks.audit("reminder.complete", &source, "ok");
                Ok(())
            }
            Err(e @ (ConnectorError::Challenge(_) | ConnectorError::Checkpoint(_))) => {
                Err(self.trip("reminder.complete", e))
            }
            Err(e) => {
                self.hooks.audit("reminder.complete", &source, "error");
                Err(CallError::Connector(e))
            }
        }
    }

    /// Create a calendar event after a human approved the proposal. Never retried.
    ///
    /// # Errors
    /// See [`CallError`].
    pub async fn create_event(&self, event: &NewEvent) -> Result<EventReceipt, CallError> {
        self.precheck(
            self.connector.capabilities().calendar_write,
            "calendar.create",
        )?;
        let source = self.connector.source();
        match self.connector.create_event(event).await {
            Ok(receipt) => {
                self.hooks.audit("calendar.create", &source, "ok");
                Ok(receipt)
            }
            Err(e @ (ConnectorError::Challenge(_) | ConnectorError::Checkpoint(_))) => {
                Err(self.trip("calendar.create", e))
            }
            Err(e) => {
                self.hooks.audit("calendar.create", &source, "error");
                Err(CallError::Connector(e))
            }
        }
    }

    /// Update a calendar event after a human approved the proposal. Never retried.
    ///
    /// # Errors
    /// See [`CallError`].
    pub async fn update_event(
        &self,
        external_id: &str,
        patch: &EventPatch,
    ) -> Result<EventReceipt, CallError> {
        self.precheck(
            self.connector.capabilities().calendar_write,
            "calendar.update",
        )?;
        let source = self.connector.source();
        match self.connector.update_event(external_id, patch).await {
            Ok(receipt) => {
                self.hooks.audit("calendar.update", &source, "ok");
                Ok(receipt)
            }
            Err(e @ (ConnectorError::Challenge(_) | ConnectorError::Checkpoint(_))) => {
                Err(self.trip("calendar.update", e))
            }
            Err(e) => {
                self.hooks.audit("calendar.update", &source, "error");
                Err(CallError::Connector(e))
            }
        }
    }

    /// Send a draft. Called only from the human send path; sends are never retried.
    ///
    /// # Errors
    /// See [`CallError`].
    pub async fn send(&self, draft: &DraftToSend) -> Result<SendReceipt, CallError> {
        self.precheck(self.connector.capabilities().send, "send")?;
        let source = self.connector.source();
        let timer = crate::log::Timer::start();
        match self.connector.send(draft).await {
            Ok(receipt) => {
                self.hooks.audit("send", &source, "ok");
                log_call(&source, "send", "ok", timer.ms(), "");
                Ok(receipt)
            }
            Err(e @ (ConnectorError::Challenge(_) | ConnectorError::Checkpoint(_))) => {
                Err(self.trip("send", e))
            }
            // The message may have left: record it distinctly, make no further call, no breaker.
            Err(e @ ConnectorError::Ambiguous(_)) => {
                self.hooks.audit("send", &source, "ambiguous");
                log_call(&source, "send", "ambiguous", timer.ms(), "ambiguous");
                Err(CallError::Connector(e))
            }
            Err(e) => {
                self.hooks.audit("send", &source, "error");
                log_call(&source, "send", "error", timer.ms(), error_class(&e));
                Err(CallError::Connector(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    struct TestConnector {
        caps: Capabilities,
        error: Option<ConnectorError>,
        calls: AtomicU32,
    }

    impl Connector for TestConnector {
        fn source(&self) -> Source {
            Source::Other("test".into())
        }
        fn capabilities(&self) -> Capabilities {
            self.caps
        }
        fn fetch(&self, _cursor: Option<Cursor>) -> BoxFuture<'_, Result<Batch, ConnectorError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let result = self.error.clone().map_or_else(|| Ok(Batch::default()), Err);
            Box::pin(async move { result })
        }
        fn send<'a>(
            &'a self,
            _draft: &'a DraftToSend,
        ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(SendReceipt {
                    external_id: "x".into(),
                })
            })
        }
    }

    #[derive(Default)]
    struct TestHooks {
        engaged: Mutex<HashSet<String>>,
        audits: Mutex<Vec<String>>,
        notices: Mutex<Vec<String>>,
    }

    impl Hooks for TestHooks {
        fn kill_switch_engaged(&self, source: &Source) -> bool {
            self.engaged.lock().unwrap().contains(&source.as_name())
        }
        fn set_kill_switch(&self, source: &Source, engaged: bool) {
            let mut set = self.engaged.lock().unwrap();
            if engaged {
                set.insert(source.as_name());
            } else {
                set.remove(&source.as_name());
            }
        }
        fn audit(&self, action: &str, _source: &Source, outcome: &str) {
            self.audits
                .lock()
                .unwrap()
                .push(format!("{action}:{outcome}"));
        }
        fn notify(&self, message: &str) {
            self.notices.lock().unwrap().push(message.to_owned());
        }
    }

    fn connector(error: Option<ConnectorError>, send: bool) -> TestConnector {
        TestConnector {
            caps: Capabilities {
                read: true,
                draft: false,
                send,
                archive: false,
                ..Default::default()
            },
            error,
            calls: AtomicU32::new(0),
        }
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut f = Box::pin(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn create_draft_is_gated_by_capability_and_default_is_unsupported() {
        let h = TestHooks::default();
        let no_draft = connector(None, false);
        let g = Guarded {
            connector: &no_draft,
            hooks: &h,
        };
        let d = DraftToSend {
            thread_id: None,
            to: None,
            body: "x".into(),
            ..Default::default()
        };
        assert_eq!(
            block_on(g.create_draft(&d)),
            Err(CallError::CapabilityMissing("draft.create"))
        );
        let with_draft = TestConnector {
            caps: Capabilities {
                read: true,
                draft: true,
                send: false,
                archive: false,
                ..Default::default()
            },
            error: None,
            calls: AtomicU32::new(0),
        };
        let g = Guarded {
            connector: &with_draft,
            hooks: &h,
        };
        // The test connector keeps the default implementation.
        assert!(matches!(
            block_on(g.create_draft(&d)),
            Err(CallError::Connector(ConnectorError::Other(_)))
        ));
    }

    #[test]
    fn reminder_calls_are_gated_by_flags_and_default_to_unsupported() {
        let h = TestHooks::default();
        let none = connector(None, false);
        let g = Guarded {
            connector: &none,
            hooks: &h,
        };
        let new = NewReminder {
            title: "t".into(),
            due: None,
            list: None,
            notes: None,
        };
        assert_eq!(
            block_on(g.create_reminder(&new)),
            Err(CallError::CapabilityMissing("reminder.create"))
        );
        assert_eq!(
            block_on(g.complete_reminder("x")),
            Err(CallError::CapabilityMissing("reminder.complete"))
        );
        assert_eq!(none.calls.load(Ordering::SeqCst), 0, "never invoked");
        let flagged = TestConnector {
            caps: Capabilities {
                read: true,
                reminders_create: true,
                reminders_complete: true,
                ..Default::default()
            },
            error: None,
            calls: AtomicU32::new(0),
        };
        let g = Guarded {
            connector: &flagged,
            hooks: &h,
        };
        // The test connector keeps the default implementations.
        assert!(matches!(
            block_on(g.create_reminder(&new)),
            Err(CallError::Connector(ConnectorError::Unsupported(_)))
        ));
        assert!(matches!(
            block_on(g.complete_reminder("x")),
            Err(CallError::Connector(ConnectorError::Unsupported(_)))
        ));
    }

    #[test]
    fn calendar_writes_are_gated_by_the_flag_and_default_to_unsupported() {
        let h = TestHooks::default();
        let none = connector(None, false);
        let g = Guarded {
            connector: &none,
            hooks: &h,
        };
        let new = NewEvent {
            calendar: "primary".into(),
            title: "t".into(),
            start: 1,
            end: 2,
            all_day: false,
            attendees: vec![],
            description: None,
        };
        assert_eq!(
            block_on(g.create_event(&new)),
            Err(CallError::CapabilityMissing("calendar.create"))
        );
        assert_eq!(
            block_on(g.update_event("c/e", &EventPatch::default())),
            Err(CallError::CapabilityMissing("calendar.update"))
        );
        assert_eq!(none.calls.load(Ordering::SeqCst), 0, "never invoked");
        assert!(
            !none.default_engaged(),
            "existing connectors keep their default"
        );
        let flagged = TestConnector {
            caps: Capabilities {
                read: true,
                calendar_write: true,
                ..Default::default()
            },
            error: None,
            calls: AtomicU32::new(0),
        };
        let g = Guarded {
            connector: &flagged,
            hooks: &h,
        };
        assert!(matches!(
            block_on(g.create_event(&new)),
            Err(CallError::Connector(ConnectorError::Unsupported(_)))
        ));
        assert!(matches!(
            block_on(g.update_event("c/e", &EventPatch::default())),
            Err(CallError::Connector(ConnectorError::Unsupported(_)))
        ));
    }

    #[test]
    fn capability_refusal_does_not_invoke_connector() {
        let c = connector(None, false);
        let h = TestHooks::default();
        let g = Guarded {
            connector: &c,
            hooks: &h,
        };
        let r = block_on(g.send(&DraftToSend {
            thread_id: None,
            to: None,
            body: "x".into(),
            ..Default::default()
        }));
        assert_eq!(r, Err(CallError::CapabilityMissing("send")));
        assert_eq!(c.calls.load(Ordering::SeqCst), 0);
        assert!(h
            .audits
            .lock()
            .unwrap()
            .iter()
            .any(|a| a == "send:refused_capability"));
    }

    #[test]
    fn successful_fetch_is_audited_without_bodies() {
        let c = connector(None, true);
        let h = TestHooks::default();
        let g = Guarded {
            connector: &c,
            hooks: &h,
        };
        assert!(block_on(g.fetch(None)).is_ok());
        assert_eq!(*h.audits.lock().unwrap(), vec!["fetch:ok".to_owned()]);
    }

    #[test]
    fn checkpoint_trips_breaker_with_zero_retries() {
        let c = connector(Some(ConnectorError::Checkpoint("verify".into())), true);
        let h = TestHooks::default();
        let g = Guarded {
            connector: &c,
            hooks: &h,
        };
        let r = block_on(g.fetch(None));
        assert!(matches!(r, Err(CallError::BreakerOpened(_))));
        assert_eq!(c.calls.load(Ordering::SeqCst), 1);
        assert!(h.kill_switch_engaged(&c.source()));
        assert_eq!(h.notices.lock().unwrap().len(), 1);
        // Further calls are refused without touching the connector.
        assert_eq!(block_on(g.fetch(None)), Err(CallError::KillSwitchEngaged));
        assert_eq!(c.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn transient_budget_is_respected() {
        let c = connector(Some(ConnectorError::Transient("net".into())), true);
        let h = TestHooks::default();
        let g = Guarded {
            connector: &c,
            hooks: &h,
        };
        let r = block_on(g.fetch(None));
        assert!(matches!(r, Err(CallError::TransientExhausted(_))));
        assert_eq!(c.calls.load(Ordering::SeqCst), TRANSIENT_ATTEMPTS_PER_CYCLE);
    }
}
