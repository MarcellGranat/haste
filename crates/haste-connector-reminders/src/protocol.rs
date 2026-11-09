//! The helper protocol. One JSON request on stdin, one JSON object on stdout. The set of
//! requests is closed: there is deliberately no delete and no edit.

use haste_core::model::{notes_snippet, Reminder};
use serde::{Deserialize, Serialize};

/// Most reminders the helper returns for one `FetchChanged` page.
pub const FETCH_PAGE_SIZE: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RemindersRequest {
    /// Report the authorization status. Only prompts when `prompt` is set (human auth command).
    Authorization { prompt: bool },
    /// Count lists (names are never needed by the daemon).
    ListLists,
    /// Reminders modified at or after `since` (0 = all), `limit` of them from `offset`, ordered
    /// by modification time then id.
    FetchChanged {
        since: i64,
        offset: usize,
        limit: usize,
        /// `false` returns open reminders only. Full sweeps use it: a real Reminders database
        /// holds years of completed items that are not worth mirroring.
        include_completed: bool,
    },
    CreateReminder {
        title: String,
        due: Option<i64>,
        list: Option<String>,
        notes: Option<String>,
    },
    /// Fails when the target is missing or already completed.
    CompleteReminder { external_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    Granted,
    Denied,
    NotDetermined,
    Restricted,
}

impl AuthStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::NotDetermined => "not_determined",
            Self::Restricted => "restricted",
        }
    }
}

/// A reminder as the helper reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireReminder {
    pub external_id: String,
    pub title: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub due: Option<i64>,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub list: String,
    #[serde(default)]
    pub modified: i64,
}

impl WireReminder {
    /// The stored record: notes are cut to the snippet bound, local fields start fresh.
    #[must_use]
    pub fn into_reminder(self) -> Reminder {
        Reminder {
            id: String::new(),
            external_id: self.external_id,
            title: self.title,
            notes_snippet: notes_snippet(&self.notes),
            due_at: self.due,
            completed: self.completed,
            list_name: self.list,
            remote_modified_at: self.modified,
            missing_since: None,
            version: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RemindersResponse {
    Authorization {
        status: AuthStatus,
    },
    ListLists {
        count: usize,
    },
    FetchChanged {
        reminders: Vec<WireReminder>,
        has_more: bool,
    },
    CreateReminder {
        reminder: WireReminder,
    },
    CompleteReminder {},
    /// `kind`: `not_authorized`, `not_found`, `already_completed`, `list_not_found`, `failed`.
    Error {
        kind: String,
        #[serde(default)]
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same fixture the Swift side checks, so schema drift fails the build on either side.
    pub(crate) const FIXTURE: &str = include_str!("../helper/fixtures/protocol.json");

    #[test]
    fn requests_have_a_stable_wire_shape() {
        let json = serde_json::to_value(RemindersRequest::FetchChanged {
            since: 5,
            offset: 0,
            limit: 200,
            include_completed: true,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"op":"fetch_changed","since":5,"offset":0,"limit":200,"include_completed":true})
        );
        let json = serde_json::to_value(RemindersRequest::Authorization { prompt: false }).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"op":"authorization","prompt":false})
        );
    }

    #[test]
    fn there_is_no_delete_or_edit_request() {
        for op in [
            "delete_reminder",
            "update_reminder",
            "edit_reminder",
            "remove",
        ] {
            let raw = format!(r#"{{"op":"{op}","external_id":"x"}}"#);
            assert!(
                serde_json::from_str::<RemindersRequest>(&raw).is_err(),
                "{op}"
            );
        }
    }

    #[test]
    fn the_shared_fixture_parses_on_both_sides() {
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        for item in fixture["requests"].as_array().unwrap() {
            let parsed: RemindersRequest = serde_json::from_value(item.clone()).unwrap();
            assert_eq!(&serde_json::to_value(&parsed).unwrap(), item);
        }
        for item in fixture["responses"].as_array().unwrap() {
            let parsed: RemindersResponse = serde_json::from_value(item.clone()).unwrap();
            assert_eq!(&serde_json::to_value(&parsed).unwrap(), item);
        }
    }

    #[test]
    fn wire_reminders_are_bounded_when_stored() {
        let wire = WireReminder {
            external_id: "e".into(),
            title: "t".into(),
            notes: "n".repeat(10_000),
            due: Some(1),
            completed: false,
            list: "L".into(),
            modified: 7,
        };
        let r = wire.into_reminder();
        assert!(r.notes_snippet.chars().count() <= haste_core::model::NOTES_SNIPPET_MAX);
        assert_eq!((r.version, r.remote_modified_at), (0, 7));
    }
}
