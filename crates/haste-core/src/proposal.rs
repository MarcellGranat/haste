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
    fn rejected_cannot_be_sent() {
        assert!(transition(Rejected, Sent).is_err());
    }

    #[test]
    fn default_ttl_is_one_week() {
        assert_eq!(DEFAULT_TTL_SECS, 604_800);
    }
}
