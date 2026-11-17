//! Unified data model shared by every source.

use serde::{Deserialize, Serialize};

/// Where a thread or message came from. Closed set plus an explicit escape hatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Source {
    Gmail,
    Icloud,
    Imessage,
    Whatsapp,
    Messenger,
    /// Google Chat, read through Beeper.
    GoogleChat,
    Email,
    /// Apple Reminders.
    Reminders,
    /// Google Calendar.
    GoogleCalendar,
    Other(String),
}

impl Source {
    /// Stable lowercase name used on the wire and in the database.
    #[must_use]
    pub fn as_name(&self) -> String {
        match self {
            Self::Gmail => "gmail".into(),
            Self::Icloud => "icloud".into(),
            Self::Imessage => "imessage".into(),
            Self::Whatsapp => "whatsapp".into(),
            Self::Messenger => "messenger".into(),
            Self::GoogleChat => "googlechat".into(),
            Self::Email => "email".into(),
            Self::Reminders => "reminders".into(),
            Self::GoogleCalendar => "gcal".into(),
            Self::Other(name) => format!("other:{name}"),
        }
    }
}

impl From<String> for Source {
    fn from(value: String) -> Self {
        match value.as_str() {
            "gmail" => Self::Gmail,
            "icloud" => Self::Icloud,
            "imessage" => Self::Imessage,
            "whatsapp" => Self::Whatsapp,
            "messenger" => Self::Messenger,
            "googlechat" => Self::GoogleChat,
            "email" => Self::Email,
            "reminders" => Self::Reminders,
            "gcal" => Self::GoogleCalendar,
            other => Self::Other(other.strip_prefix("other:").unwrap_or(other).to_owned()),
        }
    }
}

impl From<Source> for String {
    fn from(value: Source) -> Self {
        value.as_name()
    }
}

/// A conversation: an email thread, a chat, a message stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub source: Source,
    pub external_id: String,
    pub subject: String,
    pub participants: Vec<String>,
    pub last_activity_at: i64,
    /// Increments by one on every content change; drives stale detection.
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub thread_id: String,
    pub source: Source,
    pub external_id: String,
    pub sender_person_id: Option<String>,
    pub sender_identifier: Option<String>,
    pub body_text: String,
    pub sent_at: i64,
    /// Written by the human themselves (known for chats; mail compares the sender with the
    /// account's own address instead).
    #[serde(default)]
    pub is_own: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierKind {
    Email,
    Phone,
    Handle,
}

impl IdentifierKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Phone => "phone",
            Self::Handle => "handle",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "email" => Some(Self::Email),
            "phone" => Some(Self::Phone),
            "handle" => Some(Self::Handle),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identifier {
    pub kind: IdentifierKind,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    pub display_name: String,
    pub identifiers: Vec<Identifier>,
}

/// What a triage row or link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Thread,
    Message,
    Person,
    Proposal,
    Reminder,
    Event,
}

impl ObjectKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Message => "message",
            Self::Person => "person",
            Self::Proposal => "proposal",
            Self::Reminder => "reminder",
            Self::Event => "event",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "thread" => Some(Self::Thread),
            "message" => Some(Self::Message),
            "person" => Some(Self::Person),
            "proposal" => Some(Self::Proposal),
            "reminder" => Some(Self::Reminder),
            "event" => Some(Self::Event),
            _ => None,
        }
    }
}

/// Longest `notes_snippet` kept, in characters. A placeholder until the human chooses (design).
pub const NOTES_SNIPPET_MAX: usize = 280;

/// A task in Apple Reminders, as haste stores it. Only the remote-owned fields live here; project
/// tags are kept apart (keyed by `external_id`) so a re-sync can never drop them. Titles and notes
/// are never written to logs or audit records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    /// EventKit `calendarItemIdentifier`; unique within the `reminders` source.
    pub external_id: String,
    pub title: String,
    pub notes_snippet: String,
    pub due_at: Option<i64>,
    pub completed: bool,
    pub list_name: String,
    pub remote_modified_at: i64,
    /// Set when a complete sweep no longer returned it; cleared when it reappears.
    pub missing_since: Option<i64>,
    /// Increases by one whenever a remote-owned field changes; drives stale detection.
    pub version: u64,
}

/// How often a reminder repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatFreq {
    Day,
    Week,
    Month,
    Year,
}

impl RepeatFreq {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "day" => Some(Self::Day),
            "week" => Some(Self::Week),
            "month" => Some(Self::Month),
            "year" => Some(Self::Year),
            _ => None,
        }
    }
}

/// A repeat rule: every `every` days, weeks, months or years.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepeatRule {
    pub freq: RepeatFreq,
    pub every: u32,
}

/// Most characters of an event description kept locally.
pub const DESCRIPTION_SNIPPET_MAX: usize = 280;

/// A calendar event mirrored from Google Calendar. Scope tags and triage state are stored apart
/// (keyed by `external_id`) so a re-sync never drops them. Titles, descriptions, locations and
/// attendee addresses are never written to logs or audit records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    /// `<calendarId>/<eventId>`; unique within the `gcal` source.
    pub external_id: String,
    pub calendar: String,
    pub title: String,
    /// Unix seconds (start of the day in UTC for all-day events).
    pub start_at: i64,
    pub end_at: i64,
    pub all_day: bool,
    /// `confirmed`, `tentative` or `cancelled`.
    pub status: String,
    /// Attendee email addresses.
    #[serde(default)]
    pub attendees: Vec<String>,
    /// Local only: never audited.
    #[serde(default)]
    pub location: String,
    /// Local only, cut to [`DESCRIPTION_SNIPPET_MAX`]: never audited.
    #[serde(default)]
    pub description_snippet: String,
    /// The provider's last-modified time (unix seconds).
    pub updated_at: i64,
    /// Set when a complete listing no longer returned it; cleared when it reappears.
    pub missing_since: Option<i64>,
    /// Increases by one whenever a remote-owned field changes; drives stale detection.
    pub version: u64,
}

impl Event {
    #[must_use]
    pub fn location_present(&self) -> bool {
        !self.location.is_empty()
    }
}

/// Cut a description down to [`DESCRIPTION_SNIPPET_MAX`] characters.
#[must_use]
pub fn description_snippet(text: &str) -> String {
    text.chars().take(DESCRIPTION_SNIPPET_MAX).collect()
}

/// Cut a notes text down to [`NOTES_SNIPPET_MAX`] characters.
#[must_use]
pub fn notes_snippet(notes: &str) -> String {
    notes.chars().take(NOTES_SNIPPET_MAX).collect()
}

/// Local triage state, stored apart from synced content so re-syncs never overwrite it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Triage {
    pub object_id: String,
    pub object_kind: ObjectKind,
    pub priority: i32,
    pub snoozed_until: Option<i64>,
    pub set_aside: bool,
    pub archived: bool,
    pub updated_at: i64,
}

impl Triage {
    #[must_use]
    pub fn new(object_id: &str, object_kind: ObjectKind) -> Self {
        Self {
            object_id: object_id.to_owned(),
            object_kind,
            priority: 0,
            snoozed_until: None,
            set_aside: false,
            archived: false,
            updated_at: 0,
        }
    }
}

/// Closed vocabulary of link types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkType {
    RepliesTo,
    About,
    References,
    Mentions,
}

impl LinkType {
    pub const ALL: [Self; 4] = [
        Self::RepliesTo,
        Self::About,
        Self::References,
        Self::Mentions,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RepliesTo => "replies_to",
            Self::About => "about",
            Self::References => "references",
            Self::Mentions => "mentions",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub from_kind: ObjectKind,
    pub from_id: String,
    pub to_kind: ObjectKind,
    pub to_id: String,
    pub link_type: LinkType,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_round_trips_through_strings() {
        for s in [Source::Gmail, Source::Icloud, Source::Other("x".into())] {
            let back = Source::from(String::from(s.clone()));
            assert_eq!(s, back);
        }
    }

    #[test]
    fn reminders_are_a_source_and_an_object_kind() {
        assert_eq!(Source::from("reminders".to_owned()), Source::Reminders);
        assert_eq!(Source::Reminders.as_name(), "reminders");
        assert_eq!(ObjectKind::parse("reminder"), Some(ObjectKind::Reminder));
        assert_eq!(ObjectKind::Reminder.as_str(), "reminder");
    }

    #[test]
    fn reminder_round_trips_through_json_and_notes_are_bounded() {
        let r = Reminder {
            id: "r1".into(),
            external_id: "x-apple-reminder://ABC".into(),
            title: "Buy milk".into(),
            notes_snippet: notes_snippet(&"n".repeat(1000)),
            due_at: Some(1_700_000_000),
            completed: false,
            list_name: "Home".into(),
            remote_modified_at: 5,
            missing_since: None,
            version: 1,
        };
        assert_eq!(r.notes_snippet.chars().count(), NOTES_SNIPPET_MAX);
        let back: Reminder = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn the_link_vocabulary_is_unchanged_by_reminders() {
        // Reminders are new endpoints, not new link types.
        let names: Vec<&str> = LinkType::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(names, ["replies_to", "about", "references", "mentions"]);
    }

    #[test]
    fn link_type_vocabulary_is_closed() {
        assert_eq!(LinkType::parse("about"), Some(LinkType::About));
        assert_eq!(LinkType::parse("friend_of"), None);
    }
}

/// How important a task is. Reminders keeps it as a number 0 to 9 (1 to 4 high, 5 medium, 6 to 9
/// low, 0 none); the four levels map onto it and back.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    #[default]
    Middle,
    High,
    Urgent,
}

impl TaskPriority {
    pub const ALL: [TaskPriority; 4] = [Self::Low, Self::Middle, Self::High, Self::Urgent];

    /// The word shown and typed.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Middle => "middle",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }

    /// The stored number: 0 low, 1 middle, 2 high, 3 urgent.
    #[must_use]
    pub fn rank(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub fn from_rank(rank: u8) -> Option<Self> {
        Self::ALL.get(usize::from(rank)).copied()
    }

    /// One step more (`up`) or less important, stopping at the ends.
    #[must_use]
    pub fn step(self, up: bool) -> Self {
        let r = self.rank();
        Self::from_rank(if up {
            r.saturating_add(1)
        } else {
            r.saturating_sub(1)
        })
        .unwrap_or(self)
    }

    /// The number Reminders gets: urgent 1, high 3, middle none (0), low 9.
    #[must_use]
    pub fn to_apple(self) -> u8 {
        match self {
            Self::Urgent => 1,
            Self::High => 3,
            Self::Middle => 0,
            Self::Low => 9,
        }
    }

    /// The level for a number Reminders reports: 1 to 2 urgent, 3 to 4 high, none or medium
    /// (0, 5) middle, 6 to 9 low.
    #[must_use]
    pub fn from_apple(n: u8) -> Self {
        match n {
            1..=2 => Self::Urgent,
            3..=4 => Self::High,
            6..=9 => Self::Low,
            _ => Self::Middle,
        }
    }
}

#[cfg(test)]
mod priority_tests {
    use super::TaskPriority;

    #[test]
    fn the_four_levels_survive_a_trip_through_reminders() {
        for p in TaskPriority::ALL {
            assert_eq!(TaskPriority::from_apple(p.to_apple()), p);
        }
        // numbers set in the Reminders app itself
        assert_eq!(TaskPriority::from_apple(5), TaskPriority::Middle);
        assert_eq!(TaskPriority::from_apple(0), TaskPriority::Middle);
        assert_eq!(TaskPriority::from_apple(4), TaskPriority::High);
        assert_eq!(TaskPriority::from_apple(7), TaskPriority::Low);
    }
}
