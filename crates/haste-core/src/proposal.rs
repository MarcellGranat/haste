//! Proposal types and the lifecycle state machine.

use serde::{Deserialize, Serialize};

use crate::policy::Action;

/// Default time a proposal may stay `pending` before it expires (one week).
pub const DEFAULT_TTL_SECS: i64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    Pending,
    Approved,
    /// A reminder write is in flight. Never re-run automatically after a crash.
    Executing,
    Sent,
    Rejected,
    Expired,
    Superseded,
}

impl ProposalState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Executing => "executing",
            Self::Sent => "sent",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Pending,
            Self::Approved,
            Self::Executing,
            Self::Sent,
            Self::Rejected,
            Self::Expired,
            Self::Superseded,
        ]
        .into_iter()
        .find(|s| s.as_str() == value)
    }

    /// States after which the originating agent receives a result message.
    #[must_use]
    pub fn notifies_agent(self) -> bool {
        matches!(self, Self::Sent | Self::Rejected | Self::Expired)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct TransitionError {
    pub from: ProposalState,
    pub to: ProposalState,
}

/// Validate a transition; returns the new state or an error leaving the old state untouched.
///
/// # Errors
/// Returns [`TransitionError`] for any transition outside the permitted set.
pub fn transition(
    from: ProposalState,
    to: ProposalState,
) -> Result<ProposalState, TransitionError> {
    use ProposalState::{Approved, Executing, Expired, Pending, Rejected, Sent, Superseded};
    let ok = matches!(
        (from, to),
        (Pending, Approved | Rejected | Expired | Superseded)
            // `Approved -> Sent` stays for drafts sent straight through a connector; reminder
            // proposals always go through `Executing` (enforced by the store).
            | (Approved, Sent | Executing | Superseded)
            | (Executing, Sent | Approved)
    );
    if ok {
        Ok(to)
    } else {
        Err(TransitionError { from, to })
    }
}

/// What the agent asked for. The action is derived from the variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    Draft {
        to: Option<String>,
        body: String,
    },
    Archive,
    Snooze {
        until: i64,
    },
    ReminderCreate {
        title: String,
        #[serde(default)]
        due: Option<i64>,
        #[serde(default)]
        list: Option<String>,
        /// The agent's herdr workspace (or the human's choice): becomes the reminder's tag.
        #[serde(default)]
        project: Option<String>,
        /// The mail thread this reminder is about; stored as an `about` link on success.
        #[serde(default)]
        thread_id: Option<String>,
    },
    ReminderComplete {
        /// Haste id of the reminder to mark done.
        reminder_id: String,
        #[serde(default)]
        project: Option<String>,
        /// The reminder's version when the proposal was made; a change makes it stale.
        #[serde(default)]
        reminder_version: Option<u64>,
    },
    /// A new Google Calendar event. Attendees are never emailed by haste (`sendUpdates=none`).
    CalendarCreate {
        /// Google calendar id.
        calendar: String,
        title: String,
        start: i64,
        end: i64,
        #[serde(default)]
        all_day: bool,
        #[serde(default)]
        attendees: Vec<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        project: Option<String>,
    },
    /// A change to an existing event, limited to the allow-list in [`EventChanges`].
    CalendarUpdate {
        /// Haste id of the event.
        event_id: String,
        changes: EventChanges,
        #[serde(default)]
        project: Option<String>,
        /// The event's version when the proposal was made; a change makes it stale.
        #[serde(default)]
        event_version: Option<u64>,
    },
}

/// What a `CalendarUpdate` may change: title, start, end, description and location. `attendees`
/// and `status` exist only so a proposal naming them can be refused with a clear error
/// instead of failing to parse; they must always be `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventChanges {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    /// Always refused.
    #[serde(default)]
    pub attendees: Option<Vec<String>>,
    /// Always refused.
    #[serde(default)]
    pub status: Option<String>,
}

impl EventChanges {
    /// # Errors
    /// A reason when the change names attendees or a status, changes nothing, or is incoherent.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.attendees.is_some() {
            return Err("attendee changes are not allowed");
        }
        if self.status.is_some() {
            return Err("status changes (including cancelling) are not allowed");
        }
        if self.title.is_none()
            && self.start.is_none()
            && self.end.is_none()
            && self.description.is_none()
            && self.location.is_none()
        {
            return Err("the update changes nothing");
        }
        if self.title.as_deref().is_some_and(|t| t.trim().is_empty()) {
            return Err("the title cannot be empty");
        }
        if let (Some(s), Some(e)) = (self.start, self.end) {
            if e <= s {
                return Err("the event must end after it starts");
            }
        }
        Ok(())
    }
}

impl Payload {
    /// Checks that do not need the store: refused fields, empty titles, impossible times.
    ///
    /// # Errors
    /// A reason suitable for a validation error.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::CalendarCreate {
                calendar,
                title,
                start,
                end,
                ..
            } => {
                if calendar.trim().is_empty() {
                    return Err("a calendar is required");
                }
                if title.trim().is_empty() {
                    return Err("the title cannot be empty");
                }
                if end <= start {
                    return Err("the event must end after it starts");
                }
                Ok(())
            }
            Self::CalendarUpdate { changes, .. } => changes.validate(),
            _ => Ok(()),
        }
    }

    /// Calendar payloads, executed on a human's explicit action like reminders.
    #[must_use]
    pub fn is_calendar(&self) -> bool {
        matches!(
            self,
            Self::CalendarCreate { .. } | Self::CalendarUpdate { .. }
        )
    }
}

impl Payload {
    #[must_use]
    pub fn action(&self) -> Action {
        match self {
            Self::Draft { .. } => Action::Draft,
            Self::Archive => Action::Archive,
            Self::Snooze { .. } => Action::Snooze,
            Self::ReminderCreate { .. } => Action::ReminderCreate,
            Self::ReminderComplete { .. } => Action::ReminderComplete,
            Self::CalendarCreate { .. } => Action::CalendarCreate,
            Self::CalendarUpdate { .. } => Action::CalendarUpdate,
        }
    }

    /// Reminder payloads are executed with an explicit human action through `executing`.
    #[must_use]
    pub fn is_reminder(&self) -> bool {
        matches!(
            self,
            Self::ReminderCreate { .. } | Self::ReminderComplete { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub idempotency_key: String,
    pub principal_id: String,
    pub payload: Payload,
    pub source_thread_id: Option<String>,
    pub source_thread_version: Option<u64>,
    pub state: ProposalState,
    pub stale: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    pub result: Option<String>,
    /// The herdr workspace label of the agent's pane when it proposed this (routing hint only).
    #[serde(default)]
    pub project: Option<String>,
    /// The herdr machine the proposing agent runs on (`local` for the hub).
    #[serde(default)]
    pub origin_machine: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ProposalState::{Approved, Executing, Expired, Pending, Rejected, Sent, Superseded};

    #[test]
    fn permitted_transitions_only() {
        let all = [
            Pending, Approved, Executing, Sent, Rejected, Expired, Superseded,
        ];
        let permitted = [
            (Pending, Approved),
            (Pending, Rejected),
            (Pending, Expired),
            (Pending, Superseded),
            (Approved, Executing),
            (Approved, Sent), // drafts sent straight through a connector
            (Approved, Superseded),
            (Executing, Sent),
            (Executing, Approved), // an execution failed before any external effect
        ];
        for from in all {
            for to in all {
                let result = transition(from, to);
                assert_eq!(
                    result.is_ok(),
                    permitted.contains(&(from, to)),
                    "{from:?}->{to:?}"
                );
            }
        }
    }

    #[test]
    fn a_proposal_in_flight_can_only_finish_or_fall_back() {
        for to in [Pending, Rejected, Expired, Superseded, Executing] {
            assert!(transition(Executing, to).is_err(), "executing -> {to:?}");
        }
        assert_eq!(ProposalState::parse("executing"), Some(Executing));
        assert!(
            !Executing.notifies_agent(),
            "the agent hears about the outcome, not the attempt"
        );
    }

    #[test]
    fn reminder_payloads_map_to_their_actions_and_old_json_still_parses() {
        let create = Payload::ReminderCreate {
            title: "t".into(),
            due: None,
            list: None,
            project: Some("haste".into()),
            thread_id: Some("th".into()),
        };
        assert_eq!(create.action(), Action::ReminderCreate);
        let complete = Payload::ReminderComplete {
            reminder_id: "r".into(),
            project: None,
            reminder_version: None,
        };
        assert_eq!(complete.action(), Action::ReminderComplete);
        assert!(create.is_reminder() && complete.is_reminder() && !Payload::Archive.is_reminder());
        // a payload stored before due/list/thread_id existed
        let old: Payload =
            serde_json::from_str(r#"{"kind":"reminder_create","title":"x","project":null}"#)
                .unwrap();
        assert_eq!(old.action(), Action::ReminderCreate);
    }

    #[test]
    fn calendar_payloads_map_to_actions_and_the_update_allow_list_is_enforced() {
        let create = Payload::CalendarCreate {
            calendar: "primary".into(),
            title: "Sync".into(),
            start: 100,
            end: 200,
            all_day: false,
            attendees: vec![],
            description: None,
            project: Some("haste".into()),
        };
        assert_eq!(create.action(), Action::CalendarCreate);
        assert!(create.is_calendar() && !create.is_reminder());
        assert_eq!(create.validate(), Ok(()));
        let update = |changes: EventChanges| Payload::CalendarUpdate {
            event_id: "e".into(),
            changes,
            project: None,
            event_version: None,
        };
        let ok = EventChanges {
            title: Some("New".into()),
            ..EventChanges::default()
        };
        assert_eq!(update(ok.clone()).action(), Action::CalendarUpdate);
        assert_eq!(update(ok).validate(), Ok(()));
        for (bad, why) in [
            (
                EventChanges {
                    attendees: Some(vec!["a@b".into()]),
                    title: Some("x".into()),
                    ..EventChanges::default()
                },
                "attendee",
            ),
            (
                EventChanges {
                    status: Some("cancelled".into()),
                    ..EventChanges::default()
                },
                "status",
            ),
            (EventChanges::default(), "nothing"),
            (
                EventChanges {
                    start: Some(5),
                    end: Some(5),
                    ..EventChanges::default()
                },
                "end after",
            ),
        ] {
            let err = update(bad).validate().unwrap_err();
            assert!(err.contains(why), "{err}");
        }
        // a field outside the allow-list does not even parse
        let bad: Result<EventChanges, _> =
            serde_json::from_str(r#"{"title":"x","organizer":"me@x"}"#);
        assert!(bad.is_err());
        let end_before_start = Payload::CalendarCreate {
            calendar: "primary".into(),
            title: "x".into(),
            start: 9,
            end: 9,
            all_day: false,
            attendees: vec![],
            description: None,
            project: None,
        };
        assert!(end_before_start.validate().is_err());
    }

    #[test]
    fn rejected_cannot_be_sent() {
        assert!(transition(Rejected, Sent).is_err());
    }

    #[test]
    fn default_ttl_is_one_week() {
        assert_eq!(DEFAULT_TTL_SECS, 604_800);
    }
}
