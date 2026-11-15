//! Pure TUI state and key handling. No terminal access, so everything here is unit-testable.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::compose::{Compose, ComposeKind, ComposeOutcome, LabelOutcome, LabelPicker};
use crate::editor;
use crate::inbox;
use haste_core::herdr::{AgentStatus, AgentStatusRecord, MachineState};
use haste_core::ignore::IgnoreList;
use haste_core::model::Message;
use haste_core::proposal::{Payload, Proposal, ProposalState};
use haste_proto::{
    ConnectorView, Event, EventView, FilterView, MachineView, ReminderView, RemindersStatusView,
    SendPreviewView, ThreadView,
};

/// Terminal-independent key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Down,
    Up,
    Left,
    Right,
    /// A control chord such as Ctrl-S, as the lowercase letter.
    Ctrl(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Tab 1: inbox of all channels plus today's TODOs.
    Today,
    /// Tab 2: the inbox by channel.
    Channels,
    /// Tab 3: the next days of the calendar.
    Calendar,
    /// Tab 4: threads set aside or snoozed.
    Later,
    /// Secondary screens, opened with 5, 6 and 7.
    Approvals,
    Connectors,
    /// The last screen: mail from muted senders (hidden everywhere else).
    Muted,
}

impl View {
    /// The four main tabs, in order.
    pub const TABS: [Self; 4] = [Self::Today, Self::Channels, Self::Calendar, Self::Later];

    #[must_use]
    pub fn is_tab(self) -> bool {
        Self::TABS.contains(&self)
    }
}

/// A triage change made with one key on a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageChange {
    Archive,
    SetAside(bool),
    /// Back into the inbox: no longer set aside, no longer snoozed.
    BringBack,
    Priority(i32),
    /// Snooze until this unix second.
    SnoozeUntil(i64),
}

/// Most turns of one repeating task looked at when filling the agenda.
const MAX_OCCURRENCES: i64 = 400;

/// One row of the Calendar tab.
#[derive(Debug, Clone, Copy)]
pub enum AgendaItem<'a> {
    Event(&'a EventView),
    /// An open TODO item with a date, at the moment of this turn of it.
    Task {
        reminder: &'a ReminderView,
        at: i64,
    },
}

impl AgendaItem<'_> {
    /// When it starts (unix seconds).
    #[must_use]
    pub fn at(&self) -> i64 {
        match self {
            Self::Event(e) => e.event.start_at,
            Self::Task { at, .. } => *at,
        }
    }
}

/// How long a task marked done stays in the list (struck through, undoable) before it is
/// completed in Reminders and removed.
pub const DONE_GRACE: Duration = Duration::from_secs(30);

/// How often the local time zone offset is read again.
const TZ_REFRESH: Duration = Duration::from_secs(600);

/// Priority a thread gets when `p` marks it.
pub const PRIORITY_ON: i32 = 1;

/// An opened thread: its messages, oldest first, and where the reader is scrolled to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reader {
    pub view: ThreadView,
    pub messages: Vec<Message>,
    /// First visible line (set while drawing, which knows the width).
    pub scroll: Cell<usize>,
    /// Half pages asked for with Ctrl-d (down) and Ctrl-u (up), applied when drawing, which
    /// knows the height. This is how a message taller than the screen is read.
    pub half_pages: Cell<isize>,
    /// `(first line, end line)` of each shown message and the height of the view, set while
    /// drawing; Ctrl-d and Ctrl-u use them to jump over several small messages.
    pub spans: std::cell::RefCell<Vec<(usize, usize)>>,
    pub view_height: Cell<usize>,
    /// The message the cursor is on.
    pub selected: usize,
    /// The account's own address (mail), to tell which messages are yours.
    pub own: Option<String>,
    /// Messages (by id) opened in full: their quoted history and recipient lists are shown.
    pub expanded: std::collections::HashSet<String>,
    /// How many of the newest messages are shown; `o` shows more. The older ones are already
    /// stored, so showing them reads nothing again.
    pub shown: usize,
    /// The sender is blocked: the thread can be read, nothing else, until it is unblocked.
    pub blocked: bool,
    /// `(shown text, address)` of the links on screen, set while drawing; the terminal makes
    /// them clickable after the frame is drawn.
    pub links: std::cell::RefCell<Vec<(String, String)>>,
}

/// The reactions offered by `+` in the reader, in the order of the digit keys.
pub const REACTIONS: [&str; 8] = ["👍", "❤️", "😂", "😮", "😢", "🙏", "🔥", "🎉"];

/// How many of the newest messages of a thread the reader shows at first, and how many more
/// each press of `o` adds.
pub const READER_WINDOW: usize = 6;

impl Reader {
    /// Index of the oldest message on screen.
    #[must_use]
    pub fn first_visible(&self) -> usize {
        // the newest `shown` items; a batch of attachment-only messages counts as one
        let mut items = 0;
        let mut at = self.messages.len();
        while at > 0 && items < self.shown {
            at = crate::chat::batch_start(&self.messages, at - 1);
            items += 1;
        }
        at
    }
}

/// The attachment menu: the files of one message, one selected; and, while choosing where to
/// save, the folder picker with the files it will save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachMenu {
    /// For each file: the message it is in and its index there (a batch spans messages).
    pub refs: Vec<(String, usize)>,
    pub files: Vec<haste_core::attachment::AttachmentLine>,
    pub selected: usize,
    /// Choosing a folder to save into: the picker and which files (by index) go there.
    pub picker: Option<(editor::FilePicker, Vec<usize>)>,
}

/// An attachment's text in a popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub name: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    /// A picture, drawn as half-block art instead of `lines`.
    pub image: Option<crate::art::Image>,
}

/// Longest line of a text preview; longer ones are wrapped (a comfortable reading width).
pub const PREVIEW_WIDTH: usize = 100;

/// Lines a preview scrolls on a page key.
pub const PREVIEW_PAGE: usize = 15;

/// Lines a page key moves in the reader.
pub const READER_PAGE: isize = 4;

/// Which pane of the Today tab has the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodayFocus {
    Inbox,
    Todo,
}

/// How the Approvals view groups proposals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Agent,
    Project,
}

/// Group name for proposals that carry no project.
pub const NO_PROJECT: &str = "no project";

/// How long a send waits after `s` before it runs, during which any key cancels it.
pub const SEND_DELAY: Duration = Duration::from_secs(5);

/// Something the key handler wants the daemon to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Quit,
    Approve {
        ids: Vec<String>,
        acknowledge_stale: bool,
    },
    Reject {
        id: String,
    },
    /// Ask the daemon who a send would go to (read-only); the answer opens the confirmation.
    PreviewSend {
        id: String,
    },
    /// Settle a proposal left `executing` (outcome unknown): `done` when the effect is visible in
    /// the mailbox or app, otherwise it returns to approved.
    Resolve {
        id: String,
        done: bool,
    },
    /// Send, carrying the digest of the recipients the human was shown.
    Send {
        id: String,
        acknowledge_stale: bool,
        recipients_digest: String,
    },
    /// Run an approved reminder proposal in Reminders.
    Execute {
        id: String,
        acknowledge_stale: bool,
    },
    /// Create a reminder from the popup (the human typed it: no approval step).
    CreateReminder(Box<crate::remind::Task>),
    /// Change a thread's triage state (local to haste; no mailbox or chat network is touched).
    Triage {
        thread_id: String,
        change: TriageChange,
    },
    /// Fetch a thread's messages to read it.
    OpenThread {
        thread_id: String,
    },
    /// New messages arrived for the thread that is open: fetch it again, keeping the place.
    /// React to a chat message with an emoji (the daemon sends it once, never retried).
    React {
        message_id: String,
        key: String,
    },
    ReloadThread {
        thread_id: String,
    },
    /// Nothing older is stored: ask the provider for older messages, with no date limit.
    LoadOlder {
        thread_id: String,
    },
    /// Mute a thread's sender (the local `.haste_ignore` list): its mail goes to the bottom,
    /// marked blocked.
    Mute {
        thread_id: String,
    },
    /// Mark a mail thread read or unread (Gmail: also at the provider).
    SetRead {
        thread_id: String,
        read: bool,
    },
    /// Delete one muted mail from the local store (the sender stays muted).
    DeleteMuted {
        thread_id: String,
    },
    /// Let a blocked sender through: out of the mute list, into the allowed list, thread read.
    Unblock {
        thread_id: String,
    },
    /// Add or remove the thread's sender (or its domain) in the priority list.
    PriorityRule {
        thread_id: String,
        add: bool,
        domain: bool,
    },
    /// Complete these reminders in Reminders (propose, approve and run, as the human).
    CompleteReminders {
        ids: Vec<String>,
    },
    /// Save attachments of a message (by index) into a folder.
    SaveAttachments {
        files: Vec<(String, usize)>,
        dir: std::path::PathBuf,
    },
    /// Fetch one attachment for a text preview.
    PreviewAttachment {
        message_id: String,
        index: usize,
    },
    /// `r` on a thread in a list: open the thread, with the reply block below it.
    OpenAndReply {
        thread_id: String,
    },
    /// Open the editor for a reply to a thread; the text becomes a draft proposal (never sent).
    Reply {
        thread_id: String,
    },
    /// Save a reply as a draft, approved by the human who wrote it (sending stays a separate,
    /// confirmed step).
    SaveReply {
        thread_id: String,
        body: String,
        attachments: Vec<std::path::PathBuf>,
    },
    /// Append a note about a thread to the notes folder (no approval: the human wrote it).
    SaveNote {
        thread_id: String,
        text: String,
        attachments: Vec<std::path::PathBuf>,
    },
    /// Create a TODO, or change an existing one (no approval: the human did it).
    SaveTodo {
        reminder_id: Option<String>,
        title: String,
        due: Option<i64>,
        notes: String,
        attachments: Vec<std::path::PathBuf>,
    },
    /// Open a link from a message in the browser.
    /// `Ctrl-G` in a form: edit this text in `$EDITOR`, then call [`App::compose_replace_text`].
    ExternalEdit {
        text: String,
    },
    /// `l`: load the candidate labels, then call [`App::show_labels`].
    OpenLabels {
        thread_id: String,
    },
    /// Put a label on a thread or take it off.
    LabelChange {
        thread_id: String,
        label: String,
        add: bool,
    },
    ReleaseConnector {
        source: String,
    },
    EngageConnector {
        source: String,
    },
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Picking an emoji to react to a chat message (`+` in the reader).
    React {
        message_id: String,
    },
    /// Typing a filter after `/`.
    Filter,
    Detail(String),
    ConfirmBatch(Vec<String>),
    ConfirmStaleApprove(String),
    ConfirmRelease(String),
    /// `d` on the Muted screen: delete this one mail here, after a yes.
    ConfirmDeleteMuted {
        thread_id: String,
        subject: String,
    },
    /// A normal send is not confirmed with a key: it runs after [`SEND_DELAY`] unless the human
    /// cancels with any key. The recipients shown are the ones the daemon resolved and the digest
    /// binds the send to them.
    SendCountdown {
        id: String,
        recipients: Vec<String>,
        subject: String,
        source: String,
        remaining: u32,
        recipients_digest: String,
        deadline: Instant,
    },
    /// A send that still needs an explicit `y`: the thread changed since the draft was written
    /// (stale), or the recipients changed after the human last looked.
    ConfirmSend {
        id: String,
        /// Every recipient, as the daemon resolved them.
        recipients: Vec<String>,
        subject: String,
        source: String,
        stale: bool,
        remaining: u32,
        recipients_digest: String,
    },
    ConfirmExecute {
        id: String,
        what: String,
        stale: bool,
    },
    ConfirmResolve {
        id: String,
        done: bool,
    },
    /// Reading a thread (see [`App::reader`]).
    Read,
    /// A note or TODO form (see [`App::compose`]).
    Compose,
    /// Choosing a label (see [`App::labeler`]).
    Label,
    /// The attachments of one message: preview, download, download all (see [`App::attach`]).
    Attachments,
    /// A text preview of an attachment in a popup (see [`App::preview`]).
    Preview,
    /// Typing a snooze (`3h`, `2d`).
    Snooze {
        thread_id: String,
        input: String,
    },
    /// The reminder popup (`app.remind` holds it).
    RemindForm,
    /// A mail thread with several people: reply to the sender or to everyone.
    ReplyChoice {
        thread_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Group {
        principal: String,
        count: usize,
        collapsed: bool,
    },
    Item(usize),
    /// The read-only group of agents that herdr reports as blocked.
    BlockedGroup {
        count: usize,
        collapsed: bool,
    },
    Blocked(usize),
}

pub struct App {
    pub view: View,
    pub mode: Mode,
    pub threads: Vec<ThreadView>,
    pub proposals: Vec<Proposal>,
    /// Preset filters from the daemon (always at least the default).
    pub filters: Vec<FilterView>,
    /// Why `filters.json` was rejected, if it was.
    pub filter_error: Option<String>,
    pub filter_index: usize,
    pub today_selected: usize,
    pub reader: Option<Reader>,
    /// The open note or TODO form.
    pub compose: Option<Box<Compose>>,
    /// The reminder popup, while `Mode::RemindForm`.
    pub remind: Option<Box<crate::remind::RemindForm>>,
    /// Every known project label, offered in the reminder popup.
    pub known_labels: Vec<String>,
    /// The open label picker.
    pub labeler: Option<LabelPicker>,
    /// The attachment menu of the message under the reader's cursor.
    pub attach: Option<AttachMenu>,
    /// The text preview of an attachment.
    pub preview: Option<Preview>,
    /// Muted senders (`.haste_ignore`), applied to the Today and Channels lists only.
    pub ignore: IgnoreList,
    /// Senders whose mail is priority, and senders let through although never read.
    pub priority_rules: IgnoreList,
    pub allowed_rules: IgnoreList,
    /// A thread just opened that is to be marked read (the loop sends it).
    pub mark_read_on_open: Option<String>,
    pub ignore_errors: Vec<String>,
    /// Tasks selected with Space in the TODO pane.
    pub todo_marked: BTreeSet<String>,
    /// Tasks marked done: shown struck through until this moment, then completed and removed.
    pub pending_done: BTreeMap<String, Instant>,
    /// Tasks being completed right now: hidden until the next list arrives.
    completing: BTreeSet<String>,
    pub today_focus: TodayFocus,
    pub todo_selected: usize,
    /// The Later tab has two panes like Today: threads and, below, TODO items with a later date.
    pub later_focus: TodayFocus,
    pub later_todo_selected: usize,
    pub later_todo_offset: Cell<usize>,
    /// Offset of the machine's local time from UTC (seconds), refreshed by the loop.
    pub tz_offset: i64,
    tz_checked: Option<Instant>,
    /// First visible rows of the scrolling lists (set while drawing).
    pub inbox_offset: Cell<usize>,
    pub todo_offset: Cell<usize>,
    pub channel_offset: Cell<usize>,
    pub later_offset: Cell<usize>,
    /// Reactions sent and not answered yet, per message: how many, and the body before them.
    pub reactions_pending: std::collections::HashMap<String, (usize, String)>,
    pub muted_offset: Cell<usize>,
    /// Rows the list on screen shows (set while drawing), for Ctrl-d and Ctrl-u.
    pub list_rows: Cell<usize>,
    pub channels_selected: usize,
    /// Inside a channel's thread list (Channels tab).
    pub channel_inner: bool,
    pub channel_thread_selected: usize,
    pub approvals_selected: usize,
    pub connectors: Vec<ConnectorView>,
    /// Reminders permission and counts (no titles); `None` until first read.
    pub reminders: Option<RemindersStatusView>,
    /// Threads being archived in the background: out of the list already, back if it fails.
    pub pending_archive: std::collections::HashSet<String>,
    pub connectors_selected: usize,
    pub machines: Vec<MachineView>,
    /// Upcoming calendar events, soonest first (read-only).
    pub events: Vec<EventView>,
    pub calendar_selected: usize,
    pub later_selected: usize,
    pub muted_selected: usize,
    /// Open reminders (read-only; add or change them through proposals).
    pub reminder_list: Vec<ReminderView>,
    pub group_by: GroupBy,
    pub blocked_collapsed: bool,
    pub filter: String,
    pub collapsed: BTreeSet<String>,
    pub marked: BTreeSet<String>,
    pub notices: Vec<String>,
    pub status: String,
    /// The loop's clock, set by [`App::tick`]; the countdown is measured against it.
    pub now: Instant,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// The subject line of a send confirmation, followed by the files that go with it.
fn with_files(subject: String, files: &[haste_proto::AttachmentInfo]) -> String {
    if files.is_empty() {
        return subject;
    }
    let list: Vec<String> = files
        .iter()
        .map(|f| {
            format!(
                "{} ({})",
                inbox::sanitize(&f.name),
                crate::chat::human_size(f.bytes as u64)
            )
        })
        .collect();
    format!("{subject} · attached: {}", list.join(", "))
}

/// One-line description of what a proposal would do.
#[must_use]
pub fn summary(p: &Proposal) -> String {
    match &p.payload {
        Payload::Draft { to, body, .. } => {
            let head: String = body.chars().take(40).collect();
            format!("draft to {}: {head}", to.as_deref().unwrap_or("(thread)"))
        }
        Payload::Archive => "archive thread".to_owned(),
        Payload::Snooze { until } => format!("snooze until {until}"),
        Payload::ReminderCreate { title, project, .. } => {
            format!(
                "reminder '{title}'{}",
                project
                    .as_ref()
                    .map(|p| format!(" [{p}]"))
                    .unwrap_or_default()
            )
        }
        Payload::ReminderComplete { reminder_id, .. } => {
            format!("complete reminder {reminder_id}")
        }
        Payload::CalendarCreate {
            title,
            calendar,
            project,
            ..
        } => format!(
            "event '{title}' on {calendar}{}",
            project
                .as_ref()
                .map(|p| format!(" [{p}]"))
                .unwrap_or_default()
        ),
        Payload::CalendarUpdate { event_id, .. } => format!("update event {event_id}"),
        // the target and the first line of the text, so the human can decide
        Payload::NoteAppend { entry } => format!("note {}", entry.preview()),
    }
}

fn is_open(p: &Proposal) -> bool {
    matches!(
        p.state,
        ProposalState::Pending | ProposalState::Approved | ProposalState::Executing
    )
}

impl App {
    #[must_use]
    pub fn new() -> Self {
        Self {
            view: View::Today,
            mode: Mode::Normal,
            threads: Vec::new(),
            proposals: Vec::new(),
            filters: vec![FilterView {
                name: "all".into(),
                ..FilterView::default()
            }],
            filter_error: None,
            filter_index: 0,
            today_selected: 0,
            reader: None,
            compose: None,
            remind: None,
            known_labels: Vec::new(),
            labeler: None,
            attach: None,
            preview: None,
            ignore: IgnoreList::default(),
            priority_rules: IgnoreList::default(),
            allowed_rules: IgnoreList::default(),
            mark_read_on_open: None,
            ignore_errors: Vec::new(),
            todo_marked: BTreeSet::new(),
            pending_done: BTreeMap::new(),
            completing: BTreeSet::new(),
            today_focus: TodayFocus::Inbox,
            todo_selected: 0,
            later_focus: TodayFocus::Inbox,
            later_todo_selected: 0,
            later_todo_offset: Cell::new(0),
            tz_offset: 0,
            tz_checked: None,
            inbox_offset: Cell::new(0),
            todo_offset: Cell::new(0),
            channel_offset: Cell::new(0),
            later_offset: Cell::new(0),
            reactions_pending: std::collections::HashMap::new(),
            muted_offset: Cell::new(0),
            list_rows: Cell::new(0),
            channels_selected: 0,
            channel_inner: false,
            channel_thread_selected: 0,
            approvals_selected: 0,
            connectors: Vec::new(),
            reminders: None,
            pending_archive: std::collections::HashSet::new(),
            connectors_selected: 0,
            machines: Vec::new(),
            events: Vec::new(),
            calendar_selected: 0,
            later_selected: 0,
            muted_selected: 0,
            reminder_list: Vec::new(),
            group_by: GroupBy::Agent,
            blocked_collapsed: false,
            filter: String::new(),
            collapsed: BTreeSet::new(),
            marked: BTreeSet::new(),
            notices: Vec::new(),
            status: String::new(),
            now: Instant::now(),
        }
    }

    /// Replace proposals with the daemon's pending/approved list.
    pub fn set_proposals(&mut self, proposals: Vec<Proposal>) {
        self.proposals = proposals.into_iter().filter(is_open).collect();
        let live: BTreeSet<&String> = self.proposals.iter().map(|p| &p.id).collect();
        self.marked.retain(|id| live.contains(id));
        self.clamp();
    }

    pub fn set_connectors(&mut self, connectors: Vec<ConnectorView>) {
        self.connectors = connectors;
        self.clamp();
    }

    pub fn set_machines(&mut self, machines: Vec<MachineView>) {
        self.machines = machines;
        self.clamp();
    }

    /// Re-read the machine's offset from UTC at most every ten minutes (a daylight-saving switch
    /// changes it), so a "tomorrow morning" is in the time the human lives in.
    pub fn refresh_tz(&mut self) {
        let stale = self.tz_checked.is_none_or(|t| t.elapsed() > TZ_REFRESH);
        if stale {
            self.tz_offset = haste_core::time::local_offset_secs();
            self.tz_checked = Some(Instant::now());
        }
    }

    pub fn set_events(&mut self, events: Vec<EventView>) {
        self.events = events;
        self.clamp();
    }

    pub fn set_reminder_list(&mut self, reminders: Vec<ReminderView>) {
        self.reminder_list = reminders;
        // the fresh list is the truth: finished completions are gone, and marks or timers of
        // tasks that no longer exist are dropped
        self.completing.clear();
        let live: BTreeSet<&str> = self
            .reminder_list
            .iter()
            .filter(|r| !r.reminder.completed)
            .map(|r| r.reminder.id.as_str())
            .collect();
        self.todo_marked.retain(|id| live.contains(id.as_str()));
        self.pending_done.retain(|id, _| live.contains(id.as_str()));
        self.clamp();
    }

    pub fn set_rules(
        &mut self,
        mute: &[String],
        priority: &[String],
        allowed: &[String],
        errors: Vec<String>,
    ) {
        self.ignore = IgnoreList::from_patterns(mute);
        self.priority_rules = IgnoreList::from_patterns(priority);
        self.allowed_rules = IgnoreList::from_patterns(allowed);
        self.ignore_errors = errors;
    }

    /// Is this thread priority: set on it, or its sender is on the priority list (unless the
    /// human took it off this one thread, which is stored as -1).
    #[must_use]
    pub fn is_priority(&self, t: &ThreadView) -> bool {
        match t.triage.priority {
            p if p > 0 => true,
            p if p < 0 => false,
            _ => self.priority_rules.is_muted(&inbox::rule_subjects(t)),
        }
    }

    /// Is the sender on the mute list? Such mail is not shown anywhere.
    #[must_use]
    pub fn is_muted(&self, t: &ThreadView) -> bool {
        self.ignore.is_muted(&inbox::rule_subjects(t))
    }

    /// Is this thread's sender blocked: (mail) not accepted yet with `b`, and not on the
    /// priority list.
    #[must_use]
    pub fn is_blocked(&self, t: &ThreadView) -> bool {
        let subjects = inbox::rule_subjects(t);
        if self.is_muted(t) {
            return true;
        }
        inbox::is_mail(&t.thread.source.as_name())
            && !t.sender_known
            && !self.allowed_rules.is_muted(&subjects)
            && !self.priority_rules.is_muted(&subjects)
    }

    pub fn set_ignore(&mut self, patterns: &[String], errors: Vec<String>) {
        self.ignore = IgnoreList::from_patterns(patterns);
        self.ignore_errors = errors;
        self.clamp();
    }

    /// Every task marked done and not yet completed, cleared; used to finish them on quit.
    pub fn take_pending_done(&mut self) -> Vec<String> {
        let ids: Vec<String> = std::mem::take(&mut self.pending_done).into_keys().collect();
        self.completing.extend(ids.iter().cloned());
        ids
    }

    pub fn set_threads(&mut self, threads: Vec<ThreadView>) {
        self.threads = threads;
        // what is being archived in the background stays out of the list until it is known
        let pending = &self.pending_archive;
        self.threads.retain(|t| !pending.contains(&t.thread.id));
        self.clamp();
    }

    /// Take a thread out of the list now; the archive itself runs in the background.
    pub fn begin_archive(&mut self, thread_id: &str) {
        self.pending_archive.insert(thread_id.to_owned());
        self.threads.retain(|t| t.thread.id != thread_id);
        self.clamp();
    }

    /// The background archive finished. On success the thread stays gone; on failure it is
    /// let back (the caller reloads the list) and the reason is shown.
    pub fn finish_archive(&mut self, thread_id: &str, result: Result<&str, String>) {
        self.pending_archive.remove(thread_id);
        self.status = match result {
            Ok(done) => done.to_owned(),
            Err(e) => format!("archive failed, back in the list: {e}"),
        };
    }

    /// Daemon change events: proposals refresh the list, notices are shown to the human.
    pub fn on_event(&mut self, event: &Event) -> Option<Effect> {
        match event {
            Event::ThreadChanged { thread_id }
                if self
                    .reader
                    .as_ref()
                    .is_some_and(|r| &r.view.thread.id == thread_id) =>
            {
                Some(Effect::ReloadThread {
                    thread_id: thread_id.clone(),
                })
            }
            Event::ProposalChanged { .. } | Event::ThreadChanged { .. } => Some(Effect::Refresh),
            // agents finishing or getting blocked in herdr are not announced here; a herdr
            // problem (a protocol mismatch) still is
            Event::Notice { message }
                if message.starts_with("[herdr:")
                    && (message.contains(" is done") || message.contains(" is blocked")) =>
            {
                None
            }
            Event::Notice { message } => {
                self.notices.push(message.clone());
                Some(Effect::Refresh)
            }
        }
    }

    fn matches_filter(&self, p: &Proposal) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let needle = self.filter.to_lowercase();
        p.principal_id.to_lowercase().contains(&needle)
            || p.id.to_lowercase().contains(&needle)
            || summary(p).to_lowercase().contains(&needle)
            || p.project
                .as_deref()
                .is_some_and(|x| x.to_lowercase().contains(&needle))
    }

    /// The group a proposal falls in for the current grouping mode.
    fn group_key(&self, p: &Proposal) -> String {
        match self.group_by {
            GroupBy::Agent => p.principal_id.clone(),
            GroupBy::Project => p.project.clone().unwrap_or_else(|| NO_PROJECT.to_owned()),
        }
    }

    /// Key under which a group's collapsed state is stored (separate per grouping mode).
    fn collapse_key(&self, group: &str) -> String {
        match self.group_by {
            GroupBy::Agent => format!("a:{group}"),
            GroupBy::Project => format!("p:{group}"),
        }
    }

    /// Agents that herdr reports as blocked, oldest first. Only connected machines contribute.
    #[must_use]
    pub fn blocked(&self) -> Vec<AgentStatusRecord> {
        let mut out: Vec<AgentStatusRecord> = self
            .machines
            .iter()
            .filter(|m| m.state == MachineState::Connected)
            .flat_map(|m| m.agents.iter().cloned())
            .filter(|a| a.status == AgentStatus::Blocked)
            .collect();
        out.sort_by(|a, b| {
            (a.status_since, &a.machine, &a.pane_id).cmp(&(b.status_since, &b.machine, &b.pane_id))
        });
        out
    }

    /// Visible rows of the Approvals view: proposal groups (by agent or by project) with their
    /// items, then the read-only Blocked agents group.
    #[must_use]
    pub fn rows(&self) -> Vec<Row> {
        let mut groups: Vec<String> = self
            .proposals
            .iter()
            .filter(|p| self.matches_filter(p))
            .map(|p| self.group_key(p))
            .collect();
        groups.sort_unstable();
        groups.dedup();
        let mut rows = Vec::new();
        for group in groups {
            let items: Vec<usize> = self
                .proposals
                .iter()
                .enumerate()
                .filter(|(_, p)| self.group_key(p) == group && self.matches_filter(p))
                .map(|(i, _)| i)
                .collect();
            let collapsed = self.collapsed.contains(&self.collapse_key(&group));
            rows.push(Row::Group {
                principal: group,
                count: items.len(),
                collapsed,
            });
            if !collapsed {
                rows.extend(items.into_iter().map(Row::Item));
            }
        }
        let blocked = self.blocked();
        rows.push(Row::BlockedGroup {
            count: blocked.len(),
            collapsed: self.blocked_collapsed,
        });
        if !self.blocked_collapsed {
            rows.extend((0..blocked.len()).map(Row::Blocked));
        }
        rows
    }

    /// Threads passing the active filter, best first: priority, then newest activity.
    fn filtered_threads(&self, include_aside: bool) -> Vec<&ThreadView> {
        let filter = self.active_filter();
        let mut out: Vec<&ThreadView> = self
            .threads
            .iter()
            .filter(|t| {
                !t.triage.archived && (include_aside || !t.triage.set_aside) && !self.is_snoozed(t)
            })
            .filter(|t| inbox::matches(filter, t))
            // a muted sender's mail is only on the last screen
            .filter(|t| !self.is_muted(t))
            .collect();
        // senders not accepted yet last; then priority; then the newest
        out.sort_by_key(|t| {
            (
                self.is_blocked(t),
                std::cmp::Reverse(self.is_priority(t)),
                std::cmp::Reverse(t.triage.priority.max(0)),
                std::cmp::Reverse(t.thread.last_activity_at),
            )
        });
        out
    }

    #[must_use]
    pub fn active_filter(&self) -> &FilterView {
        &self.filters[self.filter_index.min(self.filters.len() - 1)]
    }

    /// Today's inbox: every channel under the active filter, nothing set aside.
    #[must_use]
    pub fn today_threads(&self) -> Vec<&ThreadView> {
        self.filtered_threads(false)
    }

    /// Start of tomorrow, and of the day after (local midnight as unix seconds).
    fn day_marks(&self) -> (i64, i64) {
        let today = inbox::day_start(haste_core::time::now_secs(), self.tz_offset);
        (today + 86_400, today + 2 * 86_400)
    }

    /// Is this TODO due tomorrow (not today, not later)?
    #[must_use]
    pub fn is_tomorrow(&self, r: &ReminderView) -> bool {
        let (tomorrow, after) = self.day_marks();
        r.reminder
            .due_at
            .is_some_and(|d| d >= tomorrow && d < after)
    }

    /// TODOs for the Today tab: open reminders due by the end of tomorrow (overdue included)
    /// or undated.
    #[must_use]
    pub fn todos(&self) -> Vec<&ReminderView> {
        let (_, end) = self.day_marks();
        let mut out: Vec<&ReminderView> = self
            .reminder_list
            .iter()
            .filter(|r| {
                !r.reminder.completed
                    && !self.completing.contains(&r.reminder.id)
                    && r.reminder.due_at.is_none_or(|d| d < end)
            })
            .collect();
        out.sort_by_key(|r| r.reminder.due_at.unwrap_or(i64::MAX));
        out
    }

    /// TODOs with a date after tomorrow: the Later tab's second pane.
    #[must_use]
    pub fn later_todos(&self) -> Vec<&ReminderView> {
        let (_, from) = self.day_marks();
        let mut out: Vec<&ReminderView> = self
            .reminder_list
            .iter()
            .filter(|r| {
                !r.reminder.completed
                    && !self.completing.contains(&r.reminder.id)
                    && r.reminder.due_at.is_some_and(|d| d >= from)
            })
            .collect();
        out.sort_by_key(|r| r.reminder.due_at.unwrap_or(i64::MAX));
        out
    }

    /// The TODO list the task keys act on here, and the cursor in it: the Today or Later TODO
    /// pane, or the one task under the cursor on the Calendar tab.
    fn active_todos(&self) -> (Vec<&ReminderView>, usize) {
        match self.view {
            View::Today => (self.todos(), self.todo_selected),
            View::Later => (self.later_todos(), self.later_todo_selected),
            View::Calendar => match self.agenda().get(self.calendar_selected) {
                Some(AgendaItem::Task { reminder, .. }) => (vec![*reminder], 0),
                _ => (Vec::new(), 0),
            },
            _ => (Vec::new(), 0),
        }
    }

    fn todo_under_cursor(&self) -> Option<&ReminderView> {
        let (list, at) = self.active_todos();
        list.get(at).copied()
    }

    /// Threads of channel `i` under the active filter (set-aside ones included, marked).
    #[must_use]
    pub fn channel_threads(&self, i: usize) -> Vec<&ThreadView> {
        let Some(name) = inbox::CHANNELS.get(i) else {
            return Vec::new();
        };
        self.filtered_threads(false)
            .into_iter()
            .filter(|t| t.thread.source.as_name() == *name)
            .collect()
    }

    /// Is the thread snoozed right now (a wake-up time still ahead)?
    #[must_use]
    pub fn is_snoozed(&self, t: &ThreadView) -> bool {
        t.triage
            .snoozed_until
            .is_some_and(|until| until > haste_core::time::now_secs())
    }

    /// The Muted screen: mail from senders on the mute list, newest first.
    #[must_use]
    pub fn muted_threads(&self) -> Vec<&ThreadView> {
        let mut out: Vec<&ThreadView> = self
            .threads
            .iter()
            .filter(|t| !t.triage.archived && self.is_muted(t))
            .collect();
        out.sort_by_key(|t| std::cmp::Reverse(t.thread.last_activity_at));
        out
    }

    /// The Later tab: threads set aside or snoozed, the ones waking soonest first, then the
    /// rest by recent activity.
    #[must_use]
    pub fn later_threads(&self) -> Vec<&ThreadView> {
        let mut out: Vec<&ThreadView> = self
            .threads
            .iter()
            .filter(|t| !t.triage.archived && (t.triage.set_aside || self.is_snoozed(t)))
            .filter(|t| !self.is_muted(t))
            .collect();
        out.sort_by_key(|t| {
            (
                t.triage
                    .snoozed_until
                    .filter(|_| self.is_snoozed(t))
                    .is_none(),
                t.triage.snoozed_until.unwrap_or(i64::MAX),
                std::cmp::Reverse(t.thread.last_activity_at),
            )
        });
        out
    }

    /// The thread the thread-keys act on, if the cursor is on one.
    #[must_use]
    pub fn selected_thread(&self) -> Option<&ThreadView> {
        match self.view {
            View::Today if self.today_focus == TodayFocus::Inbox => {
                self.today_threads().get(self.today_selected).copied()
            }
            View::Channels if self.channel_inner => self
                .channel_threads(self.channels_selected)
                .get(self.channel_thread_selected)
                .copied(),
            View::Later if self.later_focus == TodayFocus::Inbox => {
                self.later_threads().get(self.later_selected).copied()
            }
            View::Muted => self.muted_threads().get(self.muted_selected).copied(),
            _ => None,
        }
    }

    /// Calendar rows for the next [`inbox::AGENDA_DAYS`] days, soonest first: events, and open
    /// TODO items with a date (a repeating one at each of its turns in the window).
    #[must_use]
    pub fn agenda(&self) -> Vec<AgendaItem<'_>> {
        let now = haste_core::time::now_secs();
        let from = inbox::day_start(now, self.tz_offset);
        let until = from + inbox::AGENDA_DAYS * 86_400;
        let mut out: Vec<AgendaItem<'_>> = self
            .events
            .iter()
            .filter(|e| {
                e.event.status != "cancelled" && e.event.end_at >= now && e.event.start_at < until
            })
            .map(AgendaItem::Event)
            .collect();
        for r in &self.reminder_list {
            let Some(first) = r.reminder.due_at.filter(|_| !r.reminder.completed) else {
                continue;
            };
            match r.repeat {
                None => {
                    // today's tasks stay listed through the day; nothing older than today
                    if first >= from && first < until {
                        out.push(AgendaItem::Task {
                            reminder: r,
                            at: first,
                        });
                    }
                }
                Some(rule) => {
                    for n in 0..MAX_OCCURRENCES {
                        let at = inbox::step_repeat(first, rule, n, self.tz_offset);
                        if at >= until {
                            break;
                        }
                        if at >= from {
                            out.push(AgendaItem::Task { reminder: r, at });
                        }
                    }
                }
            }
        }
        out.sort_by_key(AgendaItem::at);
        out
    }

    fn clamp(&mut self) {
        let rows = self.rows().len();
        self.approvals_selected = self.approvals_selected.min(rows.saturating_sub(1));
        self.today_selected = self
            .today_selected
            .min(self.today_threads().len().saturating_sub(1));
        self.todo_selected = self.todo_selected.min(self.todos().len().saturating_sub(1));
        self.channels_selected = self.channels_selected.min(inbox::CHANNELS.len() - 1);
        self.channel_thread_selected = self.channel_thread_selected.min(
            self.channel_threads(self.channels_selected)
                .len()
                .saturating_sub(1),
        );
        self.connectors_selected = self
            .connectors_selected
            .min(self.connectors.len().saturating_sub(1));
        self.calendar_selected = self
            .calendar_selected
            .min(self.agenda().len().saturating_sub(1));
        self.later_selected = self
            .later_selected
            .min(self.later_threads().len().saturating_sub(1));
        self.muted_selected = self
            .muted_selected
            .min(self.muted_threads().len().saturating_sub(1));
        self.later_todo_selected = self
            .later_todo_selected
            .min(self.later_todos().len().saturating_sub(1));
    }

    fn selected_proposal(&self) -> Option<&Proposal> {
        match self.rows().get(self.approvals_selected)? {
            Row::Item(i) => self.proposals.get(*i),
            Row::Group { .. } | Row::BlockedGroup { .. } | Row::Blocked(_) => None,
        }
    }

    fn selection_len(&self) -> usize {
        match self.view {
            View::Today if self.today_focus == TodayFocus::Todo => self.todos().len(),
            View::Today => self.today_threads().len(),
            View::Channels if self.channel_inner => {
                self.channel_threads(self.channels_selected).len()
            }
            View::Channels => inbox::CHANNELS.len(),
            View::Calendar => self.agenda().len(),
            View::Later if self.later_focus == TodayFocus::Todo => self.later_todos().len(),
            View::Later => self.later_threads().len(),
            View::Muted => self.muted_threads().len(),
            View::Approvals => self.rows().len(),
            View::Connectors => self.connectors.len(),
        }
    }

    fn selection_mut(&mut self) -> &mut usize {
        match self.view {
            View::Today if self.today_focus == TodayFocus::Todo => &mut self.todo_selected,
            View::Today => &mut self.today_selected,
            View::Channels if self.channel_inner => &mut self.channel_thread_selected,
            View::Channels => &mut self.channels_selected,
            View::Calendar => &mut self.calendar_selected,
            View::Later if self.later_focus == TodayFocus::Todo => &mut self.later_todo_selected,
            View::Later => &mut self.later_selected,
            View::Muted => &mut self.muted_selected,
            View::Approvals => &mut self.approvals_selected,
            View::Connectors => &mut self.connectors_selected,
        }
    }

    /// Half the rows of the list on screen, at least one.
    fn half_page(&self) -> isize {
        isize::try_from((self.list_rows.get() / 2).max(1)).unwrap_or(1)
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.selection_len();
        let sel = self.selection_mut();
        *sel = if len == 0 {
            0
        } else {
            sel.saturating_add_signed(delta).min(len - 1)
        };
        if self.view == View::Channels && !self.channel_inner {
            self.channel_thread_selected = 0;
        }
    }

    fn jump(&mut self, last: bool) {
        let len = self.selection_len();
        *self.selection_mut() = if last { len.saturating_sub(1) } else { 0 };
    }

    /// `f`: next preset filter, wrapping to the first; never writes the file.
    fn cycle_filter(&mut self) {
        self.filter_index = (self.filter_index + 1) % self.filters.len();
        self.today_selected = 0;
        self.channel_thread_selected = 0;
        self.status = format!("filter: {}", self.active_filter().name);
        self.clamp();
    }

    pub fn set_filters(&mut self, filters: Vec<FilterView>, error: Option<String>) {
        // A rejected file keeps the previous valid set active.
        if error.is_none() && !filters.is_empty() {
            let name = self.active_filter().name.clone();
            self.filter_index = filters.iter().position(|f| f.name == name).unwrap_or(0);
            self.filters = filters;
        }
        self.filter_error = error;
    }

    fn toggle_group(&mut self) {
        match self.rows().get(self.approvals_selected).cloned() {
            Some(Row::Group { principal, .. }) => {
                let key = self.collapse_key(&principal);
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
                self.clamp();
            }
            Some(Row::BlockedGroup { .. }) => {
                self.blocked_collapsed = !self.blocked_collapsed;
                self.clamp();
            }
            _ => {}
        }
    }

    /// Switch between grouping proposals by agent and by project.
    fn toggle_group_by(&mut self) {
        self.group_by = match self.group_by {
            GroupBy::Agent => GroupBy::Project,
            GroupBy::Project => GroupBy::Agent,
        };
        self.approvals_selected = 0;
        self.status = match self.group_by {
            GroupBy::Agent => "grouped by agent".into(),
            GroupBy::Project => "grouped by project".into(),
        };
    }

    /// Handle one key press; returns an effect for the caller to run against the daemon.
    pub fn handle_key(&mut self, key: Key) -> Option<Effect> {
        match self.mode.clone() {
            Mode::Filter => self.key_filter(key),
            Mode::Read => self.key_read(key),
            Mode::Compose => self.key_compose(key),
            Mode::Label => self.key_label(key),
            Mode::React { message_id } => self.key_react(key, &message_id),
            Mode::Attachments => self.key_attachments(key),
            Mode::Preview => self.key_preview(key),
            Mode::Snooze { thread_id, input } => self.key_snooze(key, thread_id, input),
            Mode::RemindForm => self.key_remind_form(key),
            Mode::ReplyChoice { thread_id } => self.key_reply_choice(key, thread_id),
            Mode::Detail(_) => {
                if matches!(key, Key::Esc | Key::Char('q') | Key::Enter) {
                    self.mode = Mode::Normal;
                }
                None
            }
            Mode::ConfirmBatch(ids) => self.confirm(
                key,
                Effect::Approve {
                    ids,
                    acknowledge_stale: false,
                },
            ),
            Mode::ConfirmRelease(source) => self.confirm(key, Effect::ReleaseConnector { source }),
            Mode::ConfirmDeleteMuted { thread_id, .. } => {
                self.confirm(key, Effect::DeleteMuted { thread_id })
            }
            Mode::ConfirmStaleApprove(id) => self.confirm(
                key,
                Effect::Approve {
                    ids: vec![id],
                    acknowledge_stale: true,
                },
            ),
            Mode::SendCountdown { .. } => {
                // any key cancels; nothing was sent
                self.mode = Mode::Normal;
                self.status = "send cancelled".into();
                None
            }
            Mode::ConfirmSend {
                id,
                stale,
                recipients_digest,
                ..
            } => self.confirm(
                key,
                Effect::Send {
                    id,
                    acknowledge_stale: stale,
                    recipients_digest,
                },
            ),
            Mode::ConfirmExecute { id, stale, .. } => self.confirm(
                key,
                Effect::Execute {
                    id,
                    acknowledge_stale: stale,
                },
            ),
            Mode::ConfirmResolve { id, done } => self.confirm(key, Effect::Resolve { id, done }),
            Mode::Normal => self.key_normal(key),
        }
    }

    fn confirm(&mut self, key: Key, effect: Effect) -> Option<Effect> {
        self.mode = Mode::Normal;
        match key {
            Key::Char('y') => Some(effect),
            _ => {
                self.status = "cancelled".into();
                None
            }
        }
    }

    fn key_filter(&mut self, key: Key) -> Option<Effect> {
        match key {
            Key::Enter => self.mode = Mode::Normal,
            Key::Esc => {
                self.filter.clear();
                self.mode = Mode::Normal;
            }
            Key::Backspace => {
                self.filter.pop();
            }
            Key::Char(c) => self.filter.push(c),
            _ => {}
        }
        self.approvals_selected = 0;
        None
    }

    fn key_normal(&mut self, key: Key) -> Option<Effect> {
        let thread_view = (self.view == View::Today && self.today_focus == TodayFocus::Inbox)
            || (self.view == View::Channels && self.channel_inner)
            || (self.view == View::Later && self.later_focus == TodayFocus::Inbox)
            || self.view == View::Muted;
        match key {
            Key::Char('q') => return Some(Effect::Quit),
            Key::Tab if self.view == View::Today => {
                self.today_focus = match self.today_focus {
                    TodayFocus::Inbox => TodayFocus::Todo,
                    TodayFocus::Todo => TodayFocus::Inbox,
                };
            }
            Key::Tab if self.view == View::Later => {
                self.later_focus = match self.later_focus {
                    TodayFocus::Inbox => TodayFocus::Todo,
                    TodayFocus::Todo => TodayFocus::Inbox,
                };
            }
            Key::Tab => {
                self.view = match View::TABS.iter().position(|v| *v == self.view) {
                    Some(i) => View::TABS[(i + 1) % View::TABS.len()],
                    None => View::Today,
                };
            }
            Key::Char('1') => self.view = View::Today,
            Key::Char('2') => self.view = View::Channels,
            Key::Char('3') => self.view = View::Calendar,
            Key::Char('4') => self.view = View::Later,
            Key::Char('5') => self.view = View::Approvals,
            Key::Char('6') => self.view = View::Connectors,
            Key::Char('7') => self.view = View::Muted,
            Key::Esc | Key::Char('h') if self.view == View::Channels && self.channel_inner => {
                self.channel_inner = false;
            }
            Key::Esc if !self.view.is_tab() => self.view = View::Today,
            Key::Enter | Key::Char('l') if self.view == View::Channels && !self.channel_inner => {
                self.channel_inner = true;
                self.channel_thread_selected = 0;
            }
            Key::Char('J') if self.view == View::Today => self.today_focus = TodayFocus::Todo,
            Key::Char('K') if self.view == View::Today => self.today_focus = TodayFocus::Inbox,
            Key::Char('J') if self.view == View::Later => self.later_focus = TodayFocus::Todo,
            Key::Char('K') if self.view == View::Later => self.later_focus = TodayFocus::Inbox,
            Key::Enter if thread_view => return self.open_selected(),
            Key::Enter if self.on_todos() => self.start_todo(),
            // a new TODO: the same popup as `t` on a mail, with nothing filled in
            Key::Char('c') if self.on_todos() => self.start_new_todo(),
            Key::Char('l') if thread_view => return self.start_labels(),
            Key::Char('f') if matches!(self.view, View::Today | View::Channels) => {
                self.cycle_filter();
            }
            Key::Char('e') if thread_view => return self.triage_archive(),
            Key::Char('x') if thread_view => return self.mute_selected(),
            Key::Char(' ') if self.on_todos() => self.toggle_todo_mark(),
            Key::Char('e') if self.on_todos() => self.mark_done(),
            Key::Char('g') if self.view == View::Later => return self.bring_back(),
            Key::Char('g') if thread_view => return self.triage_aside(),
            Key::Char('p') if thread_view => return self.triage_priority(),
            Key::Char('P') if thread_view => return self.priority_sender(false),
            Key::Ctrl('p') if thread_view => return self.priority_sender(true),
            Key::Char('u') if thread_view => return self.toggle_read(),
            Key::Char('b') if thread_view => return self.unblock_selected(),
            Key::Char('d') if self.view == View::Muted => return self.start_delete_muted(),
            Key::Ctrl('r') => return Some(Effect::Refresh),
            Key::Char('s') if thread_view => self.start_snooze(),
            Key::Char('t') if thread_view => self.start_remind(),
            Key::Char('r') if thread_view => return self.start_reply(),
            Key::Char('0') => self.jump(false),
            Key::Char('P') if self.view == View::Approvals => self.toggle_group_by(),
            Key::Char('R') if self.view == View::Connectors => self.start_release(),
            Key::Char('E') if self.view == View::Connectors => return self.engage_selected(),
            Key::Char('j') | Key::Down => self.move_selection(1),
            Key::Char('k') | Key::Up => self.move_selection(-1),
            // half a screen of rows
            Key::Ctrl('d') => self.move_selection(self.half_page()),
            Key::Ctrl('u') => self.move_selection(-self.half_page()),
            Key::Char('g') if !thread_view => self.jump(false),
            Key::Char('G') => self.jump(true),
            Key::Char('r') if self.view == View::Approvals => return self.reject_selected(),
            Key::Char('u') => return Some(Effect::Refresh),
            Key::Char('/') if self.view == View::Approvals => {
                self.mode = Mode::Filter;
                self.filter.clear();
            }
            Key::Char('x' | ' ') if self.view == View::Approvals => self.toggle_mark(),
            Key::Char('c') if self.view == View::Approvals => self.toggle_group(),
            Key::Enter if self.view == View::Approvals => self.enter(),
            Key::Char('a') if self.view == View::Approvals => return self.approve_selected(),
            Key::Char('A') if self.view == View::Approvals => self.start_batch(),
            Key::Char('s') if self.view == View::Approvals => return self.start_send(),
            Key::Char('m') if self.view == View::Approvals => self.start_resolve(true),
            Key::Char('b') if self.view == View::Approvals => self.start_resolve(false),
            _ => {}
        }
        None
    }

    fn enter(&mut self) {
        match self.rows().get(self.approvals_selected).cloned() {
            Some(Row::Group { .. } | Row::BlockedGroup { .. }) => self.toggle_group(),
            Some(Row::Item(i)) => {
                if let Some(p) = self.proposals.get(i) {
                    self.mode = Mode::Detail(p.id.clone());
                }
            }
            // Blocked agents are read-only: only where they are is shown, no action is offered.
            Some(Row::Blocked(i)) => {
                if let Some(a) = self.blocked().get(i) {
                    self.status = format!(
                        "blocked agent: {} · workspace '{}' · {} (read-only)",
                        a.machine,
                        a.workspace_label,
                        a.agent.as_deref().unwrap_or("agent")
                    );
                }
            }
            None => {}
        }
    }

    fn toggle_mark(&mut self) {
        let Some(p) = self.selected_proposal() else {
            return;
        };
        let id = p.id.clone();
        if !self.marked.remove(&id) {
            self.marked.insert(id);
        }
    }

    fn reject_selected(&mut self) -> Option<Effect> {
        let (id, state) = {
            let p = self.selected_proposal()?;
            (p.id.clone(), p.state)
        };
        if state != ProposalState::Pending {
            self.status = "only pending proposals can be rejected".into();
            return None;
        }
        Some(Effect::Reject { id })
    }

    fn approve_selected(&mut self) -> Option<Effect> {
        let (id, state, stale) = {
            let p = self.selected_proposal()?;
            (p.id.clone(), p.state, p.stale)
        };
        if state != ProposalState::Pending {
            self.status = "already approved".into();
            return None;
        }
        if stale {
            self.status =
                "thread changed since this proposal; press y to acknowledge and approve".into();
            self.mode = Mode::ConfirmStaleApprove(id);
            return None;
        }
        Some(Effect::Approve {
            ids: vec![id],
            acknowledge_stale: false,
        })
    }

    /// Batch approval: marked, pending, non-stale proposals only, after one confirmation.
    fn start_batch(&mut self) {
        let mut ids = Vec::new();
        let mut stale = 0;
        for p in self
            .proposals
            .iter()
            .filter(|p| self.marked.contains(&p.id))
        {
            if p.state != ProposalState::Pending {
                continue;
            }
            if p.stale {
                stale += 1;
            } else {
                ids.push(p.id.clone());
            }
        }
        if stale > 0 {
            self.status = format!("{stale} stale proposal(s) excluded; approve them individually");
        }
        if ids.is_empty() {
            if stale == 0 {
                self.status = "nothing marked to approve".into();
            }
            return;
        }
        self.mode = Mode::ConfirmBatch(ids);
    }

    /// Releasing needs a `y`: the connector starts talking to the provider again.
    fn start_release(&mut self) {
        let Some(c) = self.connectors.get(self.connectors_selected) else {
            return;
        };
        if !c.engaged {
            self.status = format!("{} is not engaged", c.source);
            return;
        }
        self.mode = Mode::ConfirmRelease(c.source.clone());
    }

    /// Engaging is the safe direction, so it needs no confirmation.
    fn engage_selected(&mut self) -> Option<Effect> {
        let c = self.connectors.get(self.connectors_selected)?;
        if c.engaged {
            self.status = format!("{} is already engaged", c.source);
            return None;
        }
        Some(Effect::EngageConnector {
            source: c.source.clone(),
        })
    }

    /// `s` on an approved draft asks the daemon for a preview; the confirmation opens when the
    /// answer arrives and names every recipient the daemon resolved.
    fn start_send(&mut self) -> Option<Effect> {
        let p = self.selected_proposal().cloned()?;
        if p.payload.is_reminder() || p.payload.is_calendar() {
            self.start_execute(&p);
            return None;
        }
        if p.state != ProposalState::Approved {
            self.status = "send is only available for approved proposals".into();
            return None;
        }
        if !matches!(p.payload, Payload::Draft { .. }) {
            self.status = "only drafts can be sent".into();
            return None;
        }
        Some(Effect::PreviewSend { id: p.id })
    }

    /// `m` / `b` on a proposal that needs a check: mark it done (it is in the mailbox or app) or
    /// return it to approved (nothing was delivered), after one confirmation.
    fn start_resolve(&mut self, done: bool) {
        let Some(p) = self.selected_proposal().cloned() else {
            return;
        };
        if p.state != ProposalState::Executing {
            self.status = "only a proposal marked NEEDS CHECK can be settled".into();
            return;
        }
        self.mode = Mode::ConfirmResolve { id: p.id, done };
    }

    /// The daemon's answer to [`Effect::PreviewSend`]: show the confirmation.
    ///
    /// A current (not stale) send starts the [`SEND_DELAY`] countdown; a stale one still asks `y`.
    pub fn show_send_preview(&mut self, id: &str, preview: SendPreviewView) {
        if preview.stale {
            self.show_confirmed_send_preview(id, preview);
            return;
        }
        self.mode = Mode::SendCountdown {
            id: id.to_owned(),
            recipients: preview.recipients,
            subject: with_files(preview.subject, &preview.attachments),
            source: preview.source,
            remaining: preview.sends_remaining,
            recipients_digest: preview.recipients_digest,
            deadline: self.now + SEND_DELAY,
        };
    }

    /// Show a send that must be confirmed with `y` (no countdown): used when the recipients changed
    /// after the human last looked, so a changed list is never sent on a timer.
    pub fn show_confirmed_send_preview(&mut self, id: &str, preview: SendPreviewView) {
        self.mode = Mode::ConfirmSend {
            id: id.to_owned(),
            recipients: preview.recipients,
            subject: with_files(preview.subject, &preview.attachments),
            source: preview.source,
            stale: preview.stale,
            remaining: preview.sends_remaining,
            recipients_digest: preview.recipients_digest,
        };
    }

    /// Advance the clock. When a send countdown has run out, the send is returned for the caller to
    /// run (once: the mode returns to normal first).
    pub fn tick(&mut self, now: Instant) -> Option<Effect> {
        self.now = now;
        let due: Vec<String> = self
            .pending_done
            .iter()
            .filter(|(_, deadline)| now >= **deadline)
            .map(|(id, _)| id.clone())
            .collect();
        if !due.is_empty() {
            for id in &due {
                self.pending_done.remove(id);
                self.completing.insert(id.clone());
            }
            return Some(Effect::CompleteReminders { ids: due });
        }
        let Mode::SendCountdown {
            deadline,
            id,
            recipients_digest,
            ..
        } = &self.mode
        else {
            return None;
        };
        if now < *deadline {
            return None;
        }
        let effect = Effect::Send {
            id: id.clone(),
            acknowledge_stale: false,
            recipients_digest: recipients_digest.clone(),
        };
        self.mode = Mode::Normal;
        Some(effect)
    }

    /// `s` on a reminder or calendar proposal: run it after one confirmation.
    fn start_execute(&mut self, p: &Proposal) {
        match p.state {
            ProposalState::Approved => {
                self.mode = Mode::ConfirmExecute {
                    id: p.id.clone(),
                    what: summary(p),
                    stale: p.stale,
                };
            }
            ProposalState::Executing => {
                self.status =
                    "outcome unknown: check the mailbox or app, then m (it is there) or b (it is not)"
                        .into();
            }
            _ => self.status = "approve it first (a), then run it (s)".into(),
        }
    }

    /// The id and subject of the thread under the cursor, or a status line saying why not.
    fn thread_under_cursor(&mut self) -> Option<(String, String)> {
        match self.selected_thread() {
            Some(t) => Some((t.thread.id.clone(), t.thread.subject.clone())),
            None => {
                self.status = "put the cursor on a thread first".into();
                None
            }
        }
    }

    fn open_selected(&mut self) -> Option<Effect> {
        let (thread_id, _) = self.thread_under_cursor()?;
        Some(Effect::OpenThread { thread_id })
    }

    /// The daemon's answer to [`Effect::OpenThread`]: show the thread, starting at the newest
    /// message.
    pub fn show_thread(
        &mut self,
        view: ThreadView,
        mut messages: Vec<Message>,
        own: Option<String>,
    ) {
        messages.sort_by_key(|m| m.sent_at);
        let selected = messages.len().saturating_sub(1);
        let thread_id = view.thread.id.clone();
        // the list knows more than the thread itself: who the sender is, and whether blocked
        let listed = self.threads.iter().find(|t| t.thread.id == thread_id);
        let blocked = listed.is_some_and(|t| self.is_blocked(t));
        let was_unread = listed.is_some_and(|t| t.unread);
        if !blocked && was_unread {
            if let Some(t) = self.thread_mut(&thread_id) {
                t.unread = false;
            }
        }
        self.reader = Some(Reader {
            view,
            messages,
            scroll: Cell::new(0),
            half_pages: Cell::new(0),
            spans: std::cell::RefCell::new(Vec::new()),
            view_height: Cell::new(0),
            selected,
            own,
            expanded: std::collections::HashSet::new(),
            links: std::cell::RefCell::new(Vec::new()),
            shown: READER_WINDOW,
            blocked,
        });
        if !blocked && was_unread {
            self.mark_read_on_open = Some(thread_id);
        }
        self.mode = Mode::Read;
    }

    /// The open thread was fetched again: take the new messages, keeping the selected message
    /// (by id) where it is and leaving the selection alone when nothing changed.
    pub fn update_thread(&mut self, mut messages: Vec<Message>) {
        let Some(reader) = &mut self.reader else {
            return;
        };
        messages.sort_by_key(|m| m.sent_at);
        let unchanged = messages.len() == reader.messages.len()
            && messages
                .iter()
                .zip(&reader.messages)
                .all(|(a, b)| a.id == b.id && a.body_text == b.body_text);
        if unchanged {
            return;
        }
        let was_last = reader.selected + 1 >= reader.messages.len();
        let keep = reader.messages.get(reader.selected).map(|m| m.id.clone());
        reader.selected = match keep.and_then(|id| messages.iter().position(|m| m.id == id)) {
            // reading the newest message: follow the conversation
            _ if was_last => messages.len().saturating_sub(1),
            Some(i) => i,
            None => reader.selected.min(messages.len().saturating_sub(1)),
        };
        reader.messages = messages;
        reader.selected = reader.selected.max(reader.first_visible());
    }

    /// Keys while choosing a reaction: a digit sends that emoji, Esc goes back.
    /// Show the reaction on the message at once; it is taken back if Beeper refuses it.
    fn begin_reaction(&mut self, message_id: &str, emoji: &str) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let Some(m) = reader.messages.iter_mut().find(|m| m.id == message_id) else {
            return;
        };
        let entry = self
            .reactions_pending
            .entry(message_id.to_owned())
            .or_insert_with(|| (0, m.body_text.clone()));
        entry.0 += 1;
        m.body_text = haste_core::reactions::add_key(&m.body_text, emoji);
    }

    /// The answer to a reaction: on a refusal the message goes back to how it was.
    pub fn finish_reaction(&mut self, message_id: &str, result: Result<(), String>) {
        let Some((count, before)) = self.reactions_pending.get_mut(message_id) else {
            return;
        };
        *count = count.saturating_sub(1);
        let (done, before) = (*count == 0, before.clone());
        match result {
            Ok(()) => self.status = "reaction sent".into(),
            Err(why) => {
                if let Some(m) = self
                    .reader
                    .as_mut()
                    .and_then(|r| r.messages.iter_mut().find(|m| m.id == message_id))
                {
                    m.body_text = before;
                }
                // the others still in flight are shown again when their answers come
                self.status = format!("reaction not sent, taken back: {why}");
                self.reactions_pending.remove(message_id);
                return;
            }
        }
        if done {
            self.reactions_pending.remove(message_id);
        }
    }

    fn key_react(&mut self, key: Key, message_id: &str) -> Option<Effect> {
        match key {
            Key::Char(c) if c.is_ascii_digit() => {
                self.mode = Mode::Read;
                let pick = usize::try_from(c.to_digit(10)?.checked_sub(1)?).ok()?;
                let emoji = REACTIONS.get(pick)?;
                self.begin_reaction(message_id, emoji);
                self.status = format!("reacted {emoji} (sending …)");
                Some(Effect::React {
                    message_id: message_id.to_owned(),
                    key: (*emoji).to_owned(),
                })
            }
            _ => {
                self.mode = Mode::Read;
                None
            }
        }
    }

    /// Keys in the attachment menu: move, `p` preview, Enter or `d` download the one, `D` or `A`
    /// download them all (each asks for a folder first), Esc closes.
    fn key_attachments(&mut self, key: Key) -> Option<Effect> {
        let menu = self.attach.as_mut()?;
        if let Some((picker, indices)) = menu.picker.as_mut() {
            match picker.handle(key) {
                editor::PickerOutcome::Open => {}
                editor::PickerOutcome::Closed => menu.picker = None,
                editor::PickerOutcome::Chosen(dir) => {
                    let effect = Effect::SaveAttachments {
                        files: indices
                            .iter()
                            .filter_map(|i| menu.refs.get(*i).cloned())
                            .collect(),
                        dir,
                    };
                    self.attach = None;
                    self.mode = Mode::Read;
                    return Some(effect);
                }
            }
            return None;
        }
        let last = menu.files.len().saturating_sub(1);
        match key {
            Key::Esc | Key::Char('q') | Key::Char('a') => {
                self.attach = None;
                self.mode = Mode::Read;
            }
            Key::Char('j') | Key::Down => menu.selected = (menu.selected + 1).min(last),
            Key::Char('k') | Key::Up => menu.selected = menu.selected.saturating_sub(1),
            Key::Char('p') | Key::Char(' ') => {
                let (message_id, index) = menu.refs.get(menu.selected)?.clone();
                return Some(Effect::PreviewAttachment { message_id, index });
            }
            Key::Enter | Key::Char('d') => {
                menu.picker = Some((
                    editor::FilePicker::for_folders(editor::scan_dirs(&download_roots())),
                    vec![menu.selected],
                ));
            }
            Key::Char('D' | 'A') => {
                menu.picker = Some((
                    editor::FilePicker::for_folders(editor::scan_dirs(&download_roots())),
                    (0..menu.files.len()).collect(),
                ));
            }
            _ => {}
        }
        None
    }

    /// The text of an attachment arrived: show it in a popup.
    pub fn show_preview(&mut self, name: &str, text: &str) {
        // a form feed is a page (or slide) break: shown as a rule with the page number
        let mut lines: Vec<String> = Vec::new();
        for (n, page) in text.split('\u{c}').enumerate() {
            if n > 0 && !page.trim().is_empty() && !page.trim_start().starts_with("── slide") {
                lines.push(String::new());
                lines.push(format!("── page {} ──", n + 1));
                lines.push(String::new());
            } else if n > 0 {
                lines.push(String::new());
            }
            for l in inbox::sanitize(page).lines() {
                let l = l.trim_end();
                if inbox::width(l) > PREVIEW_WIDTH {
                    let indent: String = l.chars().take_while(|c| *c == ' ').collect();
                    lines.extend(
                        inbox::wrap(l.trim_start(), PREVIEW_WIDTH - indent.len().min(40))
                            .into_iter()
                            .map(|w| format!("{indent}{w}")),
                    );
                } else {
                    lines.push(l.to_owned());
                }
            }
        }
        self.preview = Some(Preview {
            name: inbox::sanitize(name).replace('\n', " "),
            lines,
            scroll: 0,
            image: None,
        });
        self.mode = Mode::Preview;
    }

    /// A picture arrived: show it as terminal art.
    pub fn show_image(&mut self, name: &str, image: crate::art::Image) {
        self.preview = Some(Preview {
            name: inbox::sanitize(name).replace('\n', " "),
            lines: Vec::new(),
            scroll: 0,
            image: Some(image),
        });
        self.mode = Mode::Preview;
    }

    /// Keys in the preview: scroll, Esc back to the menu.
    fn key_preview(&mut self, key: Key) -> Option<Effect> {
        let p = self.preview.as_mut()?;
        let last = p.lines.len().saturating_sub(1);
        match key {
            Key::Esc | Key::Char('q' | 'p') | Key::Enter => {
                self.preview = None;
                self.mode = if self.attach.is_some() {
                    Mode::Attachments
                } else {
                    Mode::Read
                };
            }
            Key::Char('j') | Key::Down => p.scroll = (p.scroll + 1).min(last),
            Key::Char('k') | Key::Up => p.scroll = p.scroll.saturating_sub(1),
            Key::Char(' ' | 'd') => p.scroll = (p.scroll + PREVIEW_PAGE).min(last),
            Key::Ctrl('d') => p.scroll = (p.scroll + PREVIEW_PAGE / 2).min(last),
            Key::Ctrl('u') => p.scroll = p.scroll.saturating_sub(PREVIEW_PAGE / 2),
            Key::Char('b' | 'u') => p.scroll = p.scroll.saturating_sub(PREVIEW_PAGE),
            Key::Char('g') => p.scroll = 0,
            Key::Char('G') => p.scroll = last,
            _ => {}
        }
        None
    }

    /// The thread just opened, when it is to be marked read (asked once).
    pub fn take_mark_read(&mut self) -> Option<String> {
        self.mark_read_on_open.take()
    }

    fn close_reader(&mut self) {
        self.reader = None;
        self.mode = Mode::Normal;
    }

    /// Keys while a thread is open: move between messages, open a link, reply, archive, close.
    fn key_read(&mut self, key: Key) -> Option<Effect> {
        let Some(reader) = &mut self.reader else {
            self.mode = Mode::Normal;
            return None;
        };
        let thread_id = reader.view.thread.id.clone();
        let last = reader.messages.len().saturating_sub(1);
        let first = reader.first_visible();
        // a batch of attachment-only messages is one item: moving lands on its last message
        if reader.blocked && matches!(key, Key::Char('r' | 'e' | 'l' | '+')) {
            self.status = "this sender is blocked: b unblocks it, Esc leaves it".into();
            return None;
        }
        let step = |r: &Reader, by: isize| {
            let base = if by > 0 {
                crate::chat::batch_end(&r.messages, r.selected)
            } else {
                crate::chat::batch_start(&r.messages, r.selected)
            };
            let to = base.saturating_add_signed(by).clamp(first, last);
            crate::chat::batch_end(&r.messages, to)
        };
        match key {
            Key::Esc | Key::Enter | Key::Char('q' | 'h') => self.close_reader(),
            Key::Char('j') | Key::Down => reader.selected = step(reader, 1),
            Key::Char('k') | Key::Up => reader.selected = step(reader, -1),
            Key::Char(' ' | 'd') => reader.selected = step(reader, READER_PAGE),
            Key::Char('u') => {
                if !inbox::is_mail(&reader.view.thread.source.as_name()) {
                    self.status = "only mail has a read mark".into();
                } else {
                    let thread_id = reader.view.thread.id.clone();
                    let read = self
                        .threads
                        .iter()
                        .find(|t| t.thread.id == thread_id)
                        .is_some_and(|t| t.unread);
                    if let Some(t) = self.thread_mut(&thread_id) {
                        t.unread = !read;
                    }
                    return Some(Effect::SetRead { thread_id, read });
                }
            }
            Key::Char('b') if reader.blocked => {
                return Some(Effect::Unblock { thread_id });
            }
            Key::Char('g' | '0') => {
                reader.selected = crate::chat::batch_end(&reader.messages, first)
            }
            Key::Char('G') => reader.selected = last,
            Key::Ctrl(c @ ('d' | 'u')) => {
                let down = c == 'd';
                let h = reader.view_height.get();
                let rel = reader.selected.saturating_sub(first);
                let span = reader.spans.borrow().get(rel).copied();
                let scroll = reader.scroll.get();
                match span {
                    // a message taller than the screen is scrolled through first
                    Some((s, e))
                        if e - s > h && (if down { scroll + h < e } else { scroll > s }) =>
                    {
                        reader
                            .half_pages
                            .set(reader.half_pages.get() + if down { 1 } else { -1 });
                    }
                    Some((s, _)) => {
                        // many small messages: jump about half a screen of them
                        let half = (h / 2).max(1);
                        let spans = reader.spans.borrow();
                        let target = if down {
                            (rel + 1..spans.len())
                                .find(|j| spans[*j].0 >= s + half)
                                .unwrap_or(spans.len().saturating_sub(1))
                        } else {
                            (0..rel)
                                .rev()
                                .find(|j| spans[*j].0 + half <= s)
                                .unwrap_or(0)
                        };
                        drop(spans);
                        reader.selected = crate::chat::batch_end(&reader.messages, first + target);
                    }
                    None => {}
                }
            }
            Key::Char('a') => {
                let from = crate::chat::batch_start(&reader.messages, reader.selected);
                let mut files = Vec::new();
                let mut refs = Vec::new();
                for m in reader
                    .messages
                    .iter()
                    .skip(from)
                    .take(crate::chat::batch_end(&reader.messages, reader.selected) + 1 - from)
                {
                    for (index, f) in
                        haste_core::attachment::lines_of(&inbox::sanitize(&m.body_text))
                            .into_iter()
                            .enumerate()
                    {
                        refs.push((m.id.clone(), index));
                        files.push(f);
                    }
                }
                match (files.is_empty(), reader.messages.get(reader.selected)) {
                    (false, Some(_)) => {
                        self.attach = Some(AttachMenu {
                            refs,
                            files,
                            selected: 0,
                            picker: None,
                        });
                        self.mode = Mode::Attachments;
                    }
                    _ => self.status = "no attachment on this message".into(),
                }
            }
            Key::Char('o') => {
                let hidden = reader.first_visible();
                if hidden == 0 {
                    // everything stored is on screen: read further back at the provider; the
                    // messages that arrive fill the extra room
                    reader.shown += READER_WINDOW;
                    self.status = "asking for older messages…".into();
                    return Some(Effect::LoadOlder { thread_id });
                } else {
                    reader.shown += READER_WINDOW;
                    self.status = format!(
                        "showing {} of {} messages",
                        reader.messages.len() - reader.first_visible(),
                        reader.messages.len()
                    );
                }
            }
            Key::Char('+') => match reader.messages.get(reader.selected).map(|m| m.id.clone()) {
                Some(_) if self.thread_is_mail(&thread_id) => {
                    self.status = "reactions are for chats, not mail".into();
                }
                Some(message_id) => self.mode = Mode::React { message_id },
                None => {}
            },
            Key::Char('x') => {
                if let Some(id) = reader.messages.get(reader.selected).map(|m| m.id.clone()) {
                    if !reader.expanded.remove(&id) {
                        reader.expanded.insert(id);
                    }
                }
            }
            Key::Char('l') => return Some(Effect::OpenLabels { thread_id }),
            Key::Char('r') => return Some(Effect::Reply { thread_id }),
            Key::Char('e') => {
                self.close_reader();
                return Some(Effect::Triage {
                    thread_id,
                    change: TriageChange::Archive,
                });
            }
            _ => {}
        }
        None
    }

    /// Is this a Gmail or iCloud thread (whose archive happens in the mailbox)?
    #[must_use]
    pub fn thread_is_mail(&self, thread_id: &str) -> bool {
        self.threads
            .iter()
            .find(|t| t.thread.id == thread_id)
            .is_some_and(|t| {
                matches!(
                    t.thread.source,
                    haste_core::model::Source::Gmail | haste_core::model::Source::Icloud
                )
            })
    }

    /// The subject and labels of a thread the TUI knows (the list or the open reader).
    #[must_use]
    pub fn thread_facts(&self, thread_id: &str) -> (String, Vec<String>) {
        if let Some(t) = self.threads.iter().find(|t| t.thread.id == thread_id) {
            return (t.thread.subject.clone(), t.tags.clone());
        }
        match &self.reader {
            Some(r) if r.view.thread.id == thread_id => {
                (r.view.thread.subject.clone(), r.view.tags.clone())
            }
            _ => (String::new(), Vec::new()),
        }
    }

    /// Where a form returns to: the reader if a thread is open, else the lists.
    fn back_mode(&self) -> Mode {
        if self.reader.is_some() {
            Mode::Read
        } else {
            Mode::Normal
        }
    }

    /// `n`: a note about the thread under the cursor (or the open one).
    /// `l`: ask for the candidate labels of the thread under the cursor.
    fn start_labels(&mut self) -> Option<Effect> {
        let (thread_id, _) = self.thread_under_cursor()?;
        Some(Effect::OpenLabels { thread_id })
    }

    /// The answer to [`Effect::OpenLabels`].
    pub fn show_labels(&mut self, thread_id: &str, candidates: Vec<String>) {
        let on = self
            .threads
            .iter()
            .find(|t| t.thread.id == thread_id)
            .map(|t| t.tags.clone())
            .unwrap_or_default();
        self.labeler = Some(LabelPicker::new(thread_id, candidates, on));
        self.mode = Mode::Label;
    }

    fn key_label(&mut self, key: Key) -> Option<Effect> {
        let picker = self.labeler.as_mut()?;
        match picker.handle(key) {
            LabelOutcome::Open => None,
            LabelOutcome::Closed => {
                self.labeler = None;
                self.mode = self.back_mode();
                None
            }
            LabelOutcome::Toggle { label, add } => {
                let thread_id = picker.thread_id.clone();
                // the picker stays open so several labels can be set; keep its own view current
                if add {
                    picker.on.push(label.clone());
                } else {
                    picker.on.retain(|l| l != &label);
                }
                Some(Effect::LabelChange {
                    thread_id,
                    label,
                    add,
                })
            }
        }
    }

    /// `Enter` on a TODO: the same popup as for a new one, filled with what it has now.
    fn start_todo(&mut self) {
        let Some(todo) = self.todo_under_cursor() else {
            return;
        };
        let form = crate::remind::RemindForm::editing(
            todo,
            &self.known_labels,
            haste_core::time::now_secs(),
            self.tz_offset,
        );
        self.remind = Some(Box::new(form));
        self.mode = Mode::RemindForm;
    }

    fn key_compose(&mut self, key: Key) -> Option<Effect> {
        let c = self.compose.as_mut()?;
        match c.handle(key) {
            ComposeOutcome::Continue => None,
            ComposeOutcome::Cancel => {
                self.compose = None;
                self.mode = self.back_mode();
                self.status = "cancelled".into();
                None
            }
            ComposeOutcome::Attach => {
                c.open_picker(editor::scan_files(&attach_roots()));
                None
            }
            ComposeOutcome::External => Some(Effect::ExternalEdit {
                text: c.focused_text(),
            }),
            ComposeOutcome::Save => self.finish_compose(),
        }
    }

    /// `Effect::Reply` was taken by the loop: open the reply form.
    pub fn open_reply_form(&mut self, thread_id: &str) {
        let (subject, _) = self.thread_facts(thread_id);
        let subject = inbox::sanitize(&subject).replace('\n', " ");
        self.compose = Some(Box::new(Compose::reply(thread_id, &subject)));
        // the thread stays above the editor, on its newest message
        if let Some(r) = self
            .reader
            .as_mut()
            .filter(|r| r.view.thread.id == thread_id)
        {
            r.selected = r.messages.len().saturating_sub(1);
        }
        self.mode = Mode::Compose;
    }

    /// The text `$EDITOR` returned goes back into the focused field.
    pub fn compose_replace_text(&mut self, text: &str) {
        if self.mode == Mode::RemindForm {
            if let Some(f) = self.remind.as_mut() {
                f.notes = text.trim_end().to_owned();
            }
            return;
        }
        if let Some(c) = self.compose.as_mut() {
            c.set_focused_text(text);
        }
    }

    fn finish_compose(&mut self) -> Option<Effect> {
        let c = self.compose.as_ref()?;
        let attachments = c.attachments.clone();
        let effect = match &c.kind {
            ComposeKind::Note { thread_id } => {
                let text = c.fields[0].area.text().trim().to_owned();
                if text.is_empty() && attachments.is_empty() {
                    self.status = "the note is empty".into();
                    return None;
                }
                Effect::SaveNote {
                    thread_id: thread_id.clone(),
                    text,
                    attachments,
                }
            }
            ComposeKind::Reply { thread_id } => {
                let body = c.fields[0].area.text().trim().to_owned();
                if body.is_empty() && attachments.is_empty() {
                    self.status = "the reply is empty".into();
                    return None;
                }
                Effect::SaveReply {
                    thread_id: thread_id.clone(),
                    body,
                    attachments,
                }
            }
            ComposeKind::Todo { reminder_id } => {
                let title = c.field("Title").map(|a| a.text()).unwrap_or_default();
                let title = title.trim().to_owned();
                if title.is_empty() {
                    self.status = "a TODO needs a title".into();
                    return None;
                }
                let due_text = c.field("Due").map(|a| a.text()).unwrap_or_default();
                let due =
                    match inbox::parse_due(&due_text, haste_core::time::now_secs(), self.tz_offset)
                    {
                        Ok(due) => due,
                        Err(e) => {
                            self.status = e;
                            return None;
                        }
                    };
                let notes = c
                    .fields
                    .last()
                    .map(|f| f.area.text().trim().to_owned())
                    .unwrap_or_default();
                Effect::SaveTodo {
                    reminder_id: reminder_id.clone(),
                    title,
                    due,
                    notes,
                    attachments,
                }
            }
        };
        self.compose = None;
        self.mode = self.back_mode();
        Some(effect)
    }

    fn on_todos(&self) -> bool {
        match self.view {
            View::Today => self.today_focus == TodayFocus::Todo,
            View::Later => self.later_focus == TodayFocus::Todo,
            View::Calendar => matches!(
                self.agenda().get(self.calendar_selected),
                Some(AgendaItem::Task { .. })
            ),
            _ => false,
        }
    }

    /// `x`: mute the sender. Their mail leaves the lists and waits on the last screen.
    fn mute_selected(&mut self) -> Option<Effect> {
        let (thread_id, _) = self.thread_under_cursor()?;
        Some(Effect::Mute { thread_id })
    }

    fn thread_mut(&mut self, id: &str) -> Option<&mut ThreadView> {
        self.threads.iter_mut().find(|t| t.thread.id == id)
    }

    /// `u`: mark the thread under the cursor read, or unread when it already is read. The list
    /// shows it at once; the provider follows.
    fn toggle_read(&mut self) -> Option<Effect> {
        let t = self.selected_thread()?;
        if !inbox::is_mail(&t.thread.source.as_name()) {
            self.status = "only mail has a read mark".into();
            return None;
        }
        let (thread_id, read) = (t.thread.id.clone(), t.unread);
        if let Some(t) = self.thread_mut(&thread_id) {
            t.unread = !read;
        }
        Some(Effect::SetRead { thread_id, read })
    }

    /// `d` on the Muted screen: ask before deleting this one mail.
    fn start_delete_muted(&mut self) -> Option<Effect> {
        let t = self.selected_thread()?;
        self.mode = Mode::ConfirmDeleteMuted {
            thread_id: t.thread.id.clone(),
            subject: inbox::sanitize(&t.thread.subject).replace('\n', " "),
        };
        None
    }

    /// `b`: let a blocked sender through.
    fn unblock_selected(&mut self) -> Option<Effect> {
        let t = self.selected_thread()?;
        if !self.is_blocked(t) {
            self.status = "this sender is not blocked".into();
            return None;
        }
        Some(Effect::Unblock {
            thread_id: t.thread.id.clone(),
        })
    }

    /// `P`: put the thread's sender on the priority list, or take it off again; with `domain`
    /// the whole domain.
    fn priority_sender(&mut self, domain: bool) -> Option<Effect> {
        let t = self.selected_thread()?;
        let on_list = self.priority_rules.is_muted(&inbox::rule_subjects(t));
        Some(Effect::PriorityRule {
            thread_id: t.thread.id.clone(),
            add: !on_list,
            domain,
        })
    }

    fn toggle_todo_mark(&mut self) {
        let Some(id) = self.todo_under_cursor().map(|r| r.reminder.id.clone()) else {
            return;
        };
        if !self.todo_marked.remove(&id) {
            self.todo_marked.insert(id);
        }
    }

    /// `e` on the TODO pane: mark the selected tasks (or the one under the cursor) done. They stay
    /// listed, struck through, for [`DONE_GRACE`] and are then completed; `e` on one of them
    /// within that time takes it back.
    fn mark_done(&mut self) {
        let under_cursor = self.todo_under_cursor().map(|r| r.reminder.id.clone());
        if self.todo_marked.is_empty() {
            let Some(id) = under_cursor else {
                return;
            };
            if self.pending_done.remove(&id).is_some() {
                self.status = "not done after all".into();
            } else {
                self.pending_done.insert(id, self.now + DONE_GRACE);
                self.status = "marked done; it is completed in 30 s (e again to undo)".into();
            }
        } else {
            let ids: Vec<String> = std::mem::take(&mut self.todo_marked).into_iter().collect();
            let n = ids.len();
            for id in ids {
                self.pending_done.insert(id, self.now + DONE_GRACE);
            }
            self.status = format!("{n} marked done; they are completed in 30 s");
        }
    }

    fn triage_archive(&mut self) -> Option<Effect> {
        let (thread_id, _) = self.thread_under_cursor()?;
        Some(Effect::Triage {
            thread_id,
            change: TriageChange::Archive,
        })
    }

    /// `g` on the Later tab: back into the inbox.
    fn bring_back(&mut self) -> Option<Effect> {
        let t = self.selected_thread()?;
        Some(Effect::Triage {
            thread_id: t.thread.id.clone(),
            change: TriageChange::BringBack,
        })
    }

    fn triage_aside(&mut self) -> Option<Effect> {
        let t = self.selected_thread()?;
        Some(Effect::Triage {
            thread_id: t.thread.id.clone(),
            change: TriageChange::SetAside(!t.triage.set_aside),
        })
    }

    fn triage_priority(&mut self) -> Option<Effect> {
        let t = self.selected_thread()?;
        let by_rule = self.priority_rules.is_muted(&inbox::rule_subjects(t));
        // taking priority off a thread whose sender is on the list sets it to -1, so the list
        // does not bring it back; otherwise back to 0
        let next = if self.is_priority(t) {
            if by_rule {
                -1
            } else {
                0
            }
        } else {
            PRIORITY_ON
        };
        Some(Effect::Triage {
            thread_id: t.thread.id.clone(),
            change: TriageChange::Priority(next),
        })
    }

    fn start_snooze(&mut self) {
        if let Some((thread_id, _)) = self.thread_under_cursor() {
            self.mode = Mode::Snooze {
                thread_id,
                input: String::new(),
            };
        }
    }

    /// `t`: the reminder popup for the thread under the cursor. The suggested title is its
    /// subject (or its sender when it has none); its project labels come along.
    fn start_remind(&mut self) {
        let Some(t) = self.selected_thread() else {
            self.status = "put the cursor on a thread first".into();
            return;
        };
        let clean = |s: &str| inbox::sanitize(s).replace('\n', " ").trim().to_owned();
        let mut suggestion = clean(&t.thread.subject);
        if suggestion.is_empty() {
            suggestion = t
                .last
                .as_ref()
                .and_then(|l| l.sender.as_deref())
                .map(|s| clean(&inbox::short_sender(s)))
                .unwrap_or_default();
        }
        self.remind = Some(Box::new(crate::remind::RemindForm::new(
            Some(&t.thread.id),
            &suggestion,
            &self.known_labels,
            &t.tags,
            haste_core::time::now_secs(),
            self.tz_offset,
        )));
        self.mode = Mode::RemindForm;
    }

    /// `c` on a TODO pane: the reminder popup with no thread and no suggested title.
    fn start_new_todo(&mut self) {
        self.remind = Some(Box::new(crate::remind::RemindForm::new(
            None,
            "",
            &self.known_labels,
            &[],
            haste_core::time::now_secs(),
            self.tz_offset,
        )));
        self.mode = Mode::RemindForm;
    }

    fn key_remind_form(&mut self, key: Key) -> Option<Effect> {
        let form = self.remind.as_mut()?;
        if key == Key::Ctrl('f') {
            form.open_picker(editor::scan_files(&attach_roots()));
            return None;
        }
        // the notes can grow long: Ctrl-g writes them in the editor ($VISUAL, $EDITOR, else nvim)
        if key == Key::Ctrl('g') && form.field == crate::remind::Field::Notes {
            return Some(Effect::ExternalEdit {
                text: form.notes.clone(),
            });
        }
        match form.handle(key) {
            crate::remind::Outcome::Continue => None,
            crate::remind::Outcome::Cancel => {
                self.remind = None;
                self.mode = Mode::Normal;
                self.status = "cancelled".into();
                None
            }
            crate::remind::Outcome::Save(task) => {
                self.remind = None;
                self.mode = Mode::Normal;
                Some(Effect::CreateReminder(task))
            }
        }
    }

    /// `r`: reply. A mail thread with several people asks first whether to reply to the sender
    /// or to everyone; anything else goes straight to the editor.
    fn start_reply(&mut self) -> Option<Effect> {
        let (several, thread_id) = {
            let t = self.selected_thread()?;
            (
                inbox::is_mail(&t.thread.source.as_name()) && t.thread.participants.len() > 2,
                t.thread.id.clone(),
            )
        };
        if several {
            self.mode = Mode::ReplyChoice { thread_id };
            None
        } else {
            Some(Effect::OpenAndReply { thread_id })
        }
    }

    fn key_snooze(&mut self, key: Key, thread_id: String, mut input: String) -> Option<Effect> {
        match key {
            Key::Esc => self.mode = Mode::Normal,
            Key::Enter => match inbox::parse_snooze(&input) {
                Ok(secs) => {
                    self.mode = Mode::Normal;
                    return Some(Effect::Triage {
                        thread_id,
                        change: TriageChange::SnoozeUntil(haste_core::time::now_secs() + secs),
                    });
                }
                Err(e) => self.status = e,
            },
            Key::Backspace => {
                input.pop();
                self.mode = Mode::Snooze { thread_id, input };
            }
            Key::Char(c) => {
                input.push(c);
                self.mode = Mode::Snooze { thread_id, input };
            }
            _ => {}
        }
        None
    }

    fn key_reply_choice(&mut self, key: Key, thread_id: String) -> Option<Effect> {
        match key {
            Key::Char('r') | Key::Enter => {
                self.mode = Mode::Normal;
                return Some(Effect::OpenAndReply { thread_id });
            }
            Key::Char('a') => {
                // The send path addresses exactly one recipient (v1), so a reply to everyone
                // cannot be sent yet. Say so instead of silently replying to one person.
                self.mode = Mode::Normal;
                self.status =
                    "reply-all is not available yet: sending addresses one recipient".into();
            }
            Key::Esc => self.mode = Mode::Normal,
            _ => {}
        }
        None
    }

    pub fn set_reminders(&mut self, status: RemindersStatusView) {
        self.reminders = Some(status);
    }

    /// Prompt line describing a pending confirmation, if any.
    #[must_use]
    pub fn confirm_prompt(&self) -> Option<String> {
        match &self.mode {
            Mode::ConfirmBatch(ids) => Some(format!("Approve {} proposal(s)? [y/N]", ids.len())),
            Mode::Snooze { input, .. } => Some(format!(
                "Snooze for (whole hours or days, e.g. 3h or 2d; Enter to set, Esc to cancel): {input}"
            )),
            Mode::ReplyChoice { .. } => {
                Some("Several people on this thread: r reply to sender · a reply to all · Esc".into())
            }
            Mode::ConfirmRelease(source) => Some(format!(
                "Release {source}? It will sync again from its saved cursor. [y/N]"
            )),
            Mode::ConfirmDeleteMuted { subject, .. } => Some(format!(
                "Delete the mail \"{subject}\" here? The mailbox is not touched and the sender stays muted. [y/N]"
            )),
            Mode::ConfirmStaleApprove(id) => {
                Some(format!("Thread changed. Approve {id} anyway? [y/N]"))
            }
            Mode::SendCountdown {
                recipients,
                subject,
                source,
                remaining,
                deadline,
                ..
            } => {
                let secs = deadline
                    .saturating_duration_since(self.now)
                    .as_millis()
                    .div_ceil(1000);
                Some(format!(
                    "SENDING to {} via {source}{} in {secs}s ({remaining} left): any key cancels",
                    recipients.join(", "),
                    if subject.is_empty() {
                        String::new()
                    } else {
                        format!(", subject '{subject}'")
                    }
                ))
            }
            Mode::ConfirmSend {
                recipients,
                subject,
                source,
                remaining,
                ..
            } => Some(format!(
                "SEND to {} via {source}{} ({remaining} left)? [y/N]",
                recipients.join(", "),
                if subject.is_empty() {
                    String::new()
                } else {
                    format!(", subject '{subject}'")
                }
            )),
            Mode::ConfirmExecute { what, .. } => Some(format!("Run: {what}? [y/N]")),
            Mode::ConfirmResolve { id, done: true } => Some(format!(
                "Mark {id} as DONE: you checked and it IS in the mailbox or app? [y/N]"
            )),
            Mode::ConfirmResolve { id, done: false } => Some(format!(
                "Return {id} to approved: you checked and NOTHING was delivered? [y/N]"
            )),
            _ => None,
        }
    }
}

/// Where the file picker looks for files to attach.
/// Where a download can go: Downloads first, then Desktop, Documents, projects, and the current
/// folder.
fn download_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        for d in ["Downloads", "Desktop", "Documents", "projects"] {
            roots.push(home.join(d));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    roots
}

fn attach_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        for d in ["Downloads", "Desktop", "Documents"] {
            roots.push(home.join(d));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(id: &str, principal: &str, state: ProposalState, stale: bool) -> Proposal {
        Proposal {
            id: id.into(),
            idempotency_key: id.into(),
            principal_id: principal.into(),
            payload: Payload::Draft {
                attachments: Vec::new(),
                to: Some("ana@x.org".into()),
                body: "hello".into(),
            },
            source_thread_id: Some("t1".into()),
            source_thread_version: Some(1),
            state,
            stale,
            created_at: 0,
            updated_at: 0,
            expires_at: 100,
            result: None,
            project: None,
            origin_machine: None,
        }
    }

    fn app_with(list: Vec<Proposal>) -> App {
        let mut app = approvals_app();
        app.set_proposals(list);
        app
    }

    fn three() -> App {
        app_with(vec![
            proposal("p1", "agent-a", ProposalState::Pending, false),
            proposal("p2", "agent-a", ProposalState::Pending, false),
            proposal("p3", "agent-b", ProposalState::Pending, false),
        ])
    }

    fn approvals_app() -> App {
        let mut app = App::new();
        app.view = View::Approvals;
        app
    }

    fn press(app: &mut App, keys: &str) -> Vec<Effect> {
        keys.chars()
            .filter_map(|c| app.handle_key(Key::Char(c)))
            .collect()
    }

    fn reminder_proposal(id: &str, state: ProposalState) -> Proposal {
        let mut p = proposal(id, "agent-a", state, false);
        p.payload = Payload::ReminderCreate {
            title: "Call Ana".into(),
            due: None,
            list: None,
            project: Some("haste".into()),
            thread_id: Some("t1".into()),
        };
        p
    }

    #[test]
    fn s_on_an_approved_reminder_asks_then_executes_and_never_sends() {
        let mut app = app_with(vec![reminder_proposal("r1", ProposalState::Approved)]);
        press(&mut app, "j"); // onto the item
        assert!(press(&mut app, "s").is_empty());
        assert!(matches!(app.mode, Mode::ConfirmExecute { .. }));
        assert!(app.confirm_prompt().unwrap().contains("Run:"));
        assert!(press(&mut app, "n").is_empty(), "anything but y cancels");
        press(&mut app, "s");
        assert_eq!(
            press(&mut app, "y"),
            vec![Effect::Execute {
                id: "r1".into(),
                acknowledge_stale: false
            }]
        );
    }

    #[test]
    fn an_executing_proposal_offers_no_rerun_only_a_hint_to_check() {
        let mut app = app_with(vec![reminder_proposal("r1", ProposalState::Executing)]);
        press(&mut app, "j");
        assert!(press(&mut app, "s").is_empty());
        assert_eq!(app.mode, Mode::Normal);
        assert!(
            app.status.contains("m (it is there) or b (it is not)"),
            "{}",
            app.status
        );
        assert_eq!(app.rows().len(), 3, "it stays listed as open");
    }

    fn mail_thread(id: &str, subject: &str, who: &[&str], t: i64) -> ThreadView {
        ThreadView {
            thread: haste_core::model::Thread {
                id: id.into(),
                source: haste_core::model::Source::Gmail,
                external_id: format!("g-{id}"),
                subject: subject.into(),
                participants: who.iter().map(|s| (*s).to_owned()).collect(),
                last_activity_at: t,
                version: 1,
            },
            triage: haste_core::model::Triage::new(id, haste_core::model::ObjectKind::Thread),
            tags: vec![],
            last: None,
            unread: false,
            replied: false,
            sender_known: true,
            sender: None,
        }
    }

    fn inbox_app() -> App {
        let mut app = App::new();
        app.set_threads(vec![
            mail_thread("t1", "Quarterly plan", &["a@x.com"], 10),
            mail_thread("t2", "Team lunch", &["a@x.com", "b@x.com", "c@x.com"], 20),
        ]);
        app
    }

    #[test]
    fn the_app_starts_on_today_with_newest_first() {
        let app = inbox_app();
        assert_eq!(app.view, View::Today);
        let ids: Vec<&str> = app
            .today_threads()
            .iter()
            .map(|t| t.thread.id.as_str())
            .collect();
        assert_eq!(ids, ["t2", "t1"]);
    }

    #[test]
    fn archive_aside_and_priority_keys_emit_triage_effects() {
        let mut app = inbox_app(); // cursor on t2
        assert_eq!(
            press(&mut app, "e"),
            vec![Effect::Triage {
                thread_id: "t2".into(),
                change: TriageChange::Archive
            }]
        );
        assert_eq!(
            press(&mut app, "g"),
            vec![Effect::Triage {
                thread_id: "t2".into(),
                change: TriageChange::SetAside(true)
            }]
        );
        assert_eq!(
            press(&mut app, "p"),
            vec![Effect::Triage {
                thread_id: "t2".into(),
                change: TriageChange::Priority(PRIORITY_ON)
            }]
        );
    }

    #[test]
    fn priority_threads_sort_first_and_aside_threads_leave_today() {
        let mut app = inbox_app();
        app.threads[0].triage.priority = 1;
        assert_eq!(app.today_threads()[0].thread.id, "t1");
        app.threads[0].triage.set_aside = true;
        assert_eq!(app.today_threads().len(), 1);
        app.threads[0].triage.archived = true;
        app.threads[0].triage.set_aside = false;
        assert_eq!(app.today_threads().len(), 1, "archived threads are hidden");
    }

    #[test]
    fn snooze_prompt_takes_whole_hours_or_days_only() {
        let mut app = inbox_app();
        press(&mut app, "s");
        assert!(matches!(app.mode, Mode::Snooze { .. }));
        press(&mut app, "1.5h");
        assert!(press(&mut app, "\n").is_empty());
        assert!(
            matches!(app.mode, Mode::Snooze { .. }),
            "stays open on a bad value"
        );
        for _ in 0..4 {
            app.handle_key(Key::Backspace);
        }
        press(&mut app, "3h");
        let effects: Vec<Effect> = app.handle_key(Key::Enter).into_iter().collect();
        assert!(matches!(
            effects.as_slice(),
            [Effect::Triage {
                change: TriageChange::SnoozeUntil(_),
                ..
            }]
        ));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn t_opens_the_reminder_popup_with_the_subject_as_the_dim_suggestion() {
        let mut app = inbox_app(); // cursor on t2, "Team lunch"
        app.known_labels = vec!["haste".into()];
        press(&mut app, "t");
        assert_eq!(app.mode, Mode::RemindForm);
        let form = app.remind.as_ref().unwrap();
        assert_eq!(
            (form.suggestion.as_str(), form.title.as_str()),
            ("Team lunch", "")
        );
        // Enter takes the suggestion and the default date (tomorrow morning)
        let Some(Effect::CreateReminder(task)) = app.handle_key(Key::Enter) else {
            panic!("Enter creates it")
        };
        assert_eq!(task.title, "Team lunch");
        assert_eq!(task.thread_id.as_deref(), Some("t2"));
        let tomorrow = inbox::tomorrow_morning(haste_core::time::now_secs(), app.tz_offset);
        assert!((task.due.unwrap() - tomorrow).abs() <= 1);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.remind.is_none());
    }

    #[test]
    fn the_popup_reads_a_date_from_the_subject_and_escape_cancels() {
        let mut app = App::new();
        app.set_threads(vec![mail_thread(
            "t9",
            "Contract due 2099-03-04",
            &["a@x.com"],
            1,
        )]);
        press(&mut app, "t");
        let Some(Effect::CreateReminder(task)) = app.handle_key(Key::Enter) else {
            panic!()
        };
        assert_eq!(
            task.due,
            Some(haste_core::time::from_civil(2099, 3, 4, 10, 0))
        );
        press(&mut app, "t");
        assert!(app.handle_key(Key::Esc).is_none());
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.remind.is_none());
    }

    #[test]
    fn dated_tasks_show_on_the_calendar_and_repeating_ones_at_each_turn() {
        use haste_core::model::{RepeatFreq, RepeatRule};
        let mut app = App::new();
        let now = haste_core::time::now_secs();
        let mut daily = reminder_view("daily", Some(now + 3_600));
        daily.repeat = Some(RepeatRule {
            freq: RepeatFreq::Day,
            every: 1,
        });
        let mut done = reminder_view("done", Some(now + 3_600));
        done.reminder.completed = true;
        app.set_reminder_list(vec![
            daily,
            reminder_view("once", Some(now + 2 * 86_400)),
            reminder_view("old", Some(now - 5 * 86_400)),
            reminder_view("undated", None),
            reminder_view("far", Some(now + 30 * 86_400)),
            done,
        ]);
        let tasks: Vec<(&str, i64)> = app
            .agenda()
            .into_iter()
            .filter_map(|x| match x {
                AgendaItem::Task { reminder, at } => Some((reminder.reminder.id.as_str(), at)),
                AgendaItem::Event(_) => None,
            })
            .collect();
        let daily_at: Vec<i64> = tasks
            .iter()
            .filter(|(id, _)| *id == "daily")
            .map(|(_, at)| *at)
            .collect();
        assert!(daily_at.len() >= 6, "a turn each day: {daily_at:?}");
        assert!(daily_at.windows(2).all(|w| w[1] - w[0] == 86_400));
        let once: Vec<_> = tasks.iter().filter(|(id, _)| *id == "once").collect();
        assert_eq!(once.len(), 1);
        for left_out in ["old", "undated", "far", "done"] {
            assert!(tasks.iter().all(|(id, _)| *id != left_out), "{left_out}");
        }
        assert!(tasks.windows(2).all(|w| w[0].1 <= w[1].1), "soonest first");
    }

    #[test]
    fn tomorrows_todos_join_today_and_later_ones_go_to_the_later_tab() {
        let mut app = App::new();
        let now = haste_core::time::now_secs();
        app.set_reminder_list(vec![
            reminder_view("today", Some(now + 60)),
            reminder_view("tomorrow", Some(now + 86_400)),
            reminder_view("later", Some(now + 3 * 86_400)),
            reminder_view("undated", None),
        ]);
        let ids = |v: Vec<&ReminderView>| -> Vec<String> {
            v.iter().map(|r| r.reminder.id.clone()).collect()
        };
        assert_eq!(ids(app.todos()), ["today", "tomorrow", "undated"]);
        assert!(app.is_tomorrow(&app.todos()[1].clone()));
        assert!(!app.is_tomorrow(&app.todos()[0].clone()));
        assert_eq!(ids(app.later_todos()), ["later"]);
        // the Later tab's TODO pane has the task keys
        press(&mut app, "4");
        press(&mut app, "J");
        assert_eq!(app.later_focus, TodayFocus::Todo);
        assert!(matches!(
            press(&mut app, "e").as_slice(),
            [] // marks it done for 30 s, no effect yet
        ));
        assert!(app.pending_done.contains_key("later"));
        press(&mut app, "K");
        assert_eq!(app.later_focus, TodayFocus::Inbox);
    }

    #[test]
    fn a_task_on_the_calendar_can_be_edited_and_marked_done() {
        let mut app = App::new();
        let now = haste_core::time::now_secs();
        app.set_reminder_list(vec![reminder_view("rent", Some(now + 3_600))]);
        press(&mut app, "3");
        assert!(app.on_todos());
        // Enter opens the same popup as for a new one, filled with what it has
        app.handle_key(Key::Enter);
        assert_eq!(app.mode, Mode::RemindForm);
        let form = app.remind.as_ref().unwrap();
        assert_eq!(form.edit_id.as_deref(), Some("rent"));
        assert_eq!(form.title, "task rent");
        assert!(form.due.is_some());
        // saving gives an edit, not a new TODO
        let Some(Effect::CreateReminder(task)) = app.handle_key(Key::Enter) else {
            panic!("expected the edit")
        };
        assert_eq!(task.reminder_id.as_deref(), Some("rent"));
        assert_eq!(app.mode, Mode::Normal);
        // Esc leaves it as it was
        app.handle_key(Key::Enter);
        assert_eq!(app.mode, Mode::RemindForm);
        app.handle_key(Key::Esc);
        assert_eq!(app.mode, Mode::Normal);
        // e marks it done, e again takes it back
        press(&mut app, "e");
        assert!(app.pending_done.contains_key("rent"));
        press(&mut app, "e");
        assert!(app.pending_done.is_empty());
    }

    #[test]
    fn ctrl_g_in_the_notes_field_hands_the_notes_to_the_editor_and_takes_them_back() {
        let mut app = inbox_app();
        press(&mut app, "t");
        // Tab: title -> date -> repeat -> notes
        for _ in 0..3 {
            app.handle_key(Key::Tab);
        }
        press(&mut app, "first");
        assert_eq!(
            app.handle_key(Key::Ctrl('g')),
            Some(Effect::ExternalEdit {
                text: "first".into()
            })
        );
        app.compose_replace_text("first\nsecond\n");
        assert_eq!(app.remind.as_ref().unwrap().notes, "first\nsecond");
        let Some(Effect::CreateReminder(task)) = app.handle_key(Key::Enter) else {
            panic!()
        };
        assert_eq!(task.notes, "first\nsecond");
    }

    #[test]
    fn capital_j_and_k_move_between_the_incoming_and_todo_panes() {
        let mut app = inbox_app();
        assert_eq!(app.today_focus, TodayFocus::Inbox);
        press(&mut app, "J");
        assert_eq!(app.today_focus, TodayFocus::Todo);
        // thread keys do nothing while the TODO pane has the cursor
        for keys in ["e", "g", "p", "s", "t", "r"] {
            assert!(press(&mut app, keys).is_empty(), "{keys}");
        }
        assert_eq!(app.mode, Mode::Normal);
        press(&mut app, "K");
        assert_eq!(app.today_focus, TodayFocus::Inbox);
        assert_eq!(press(&mut app, "e").len(), 1);
    }

    #[test]
    fn the_incoming_list_scrolls_past_ten_rows() {
        let mut app = App::new();
        app.set_threads(
            (0..25)
                .map(|i| mail_thread(&format!("t{i}"), "s", &["a@x.com"], i))
                .collect(),
        );
        assert_eq!(app.today_threads().len(), 25);
        press(&mut app, "G");
        assert_eq!(app.today_selected, 24);
    }

    #[test]
    fn x_mutes_and_the_muted_mail_waits_on_the_last_screen() {
        let mut app = inbox_app();
        app.threads[0].sender = Some("a@x.com".into());
        app.threads[1].sender = Some("b@x.com".into());
        assert_eq!(
            press(&mut app, "x"),
            vec![Effect::Mute {
                thread_id: "t2".into()
            }]
        );
        // the daemon answers with the pattern; the next refresh delivers the list
        app.set_ignore(&["^b@x\\.com$".to_owned()], vec![]);
        let ids = |v: Vec<&ThreadView>| -> Vec<String> {
            v.iter().map(|t| t.thread.id.clone()).collect()
        };
        assert_eq!(ids(app.today_threads()), ["t1"], "not in the inbox");
        press(&mut app, "2");
        assert_eq!(app.channel_threads(0).len(), 1);
        assert_eq!(ids(app.muted_threads()), ["t2"]);
        // the last screen: Esc goes back, b takes the sender off the mute list
        press(&mut app, "7");
        assert_eq!(app.view, View::Muted);
        assert_eq!(
            press(&mut app, "b"),
            vec![Effect::Unblock {
                thread_id: "t2".into()
            }]
        );
        app.handle_key(Key::Esc);
        assert_eq!(app.view, View::Today);
        // an invalid line is reported and the rest still applies
        app.set_ignore(
            &["^b@x\\.com$".to_owned()],
            vec!["line 3: not a valid regular expression".into()],
        );
        assert_eq!(app.ignore_errors.len(), 1);
    }

    #[test]
    fn d_on_the_muted_screen_asks_before_deleting_this_one_mail() {
        let mut app = inbox_app();
        app.threads[1].sender = Some("b@x.com".into());
        app.set_ignore(&["^b@x\\.com$".to_owned()], vec![]);
        // d does nothing outside the Muted screen
        assert!(press(&mut app, "d").is_empty());
        assert_eq!(app.mode, Mode::Normal);
        press(&mut app, "7");
        assert!(press(&mut app, "d").is_empty());
        assert!(matches!(app.mode, Mode::ConfirmDeleteMuted { .. }));
        let prompt = app.confirm_prompt().unwrap();
        assert!(
            prompt.contains("Team lunch") && prompt.contains("not touched"),
            "{prompt}"
        );
        // anything but y cancels
        assert!(press(&mut app, "n").is_empty());
        assert_eq!(app.mode, Mode::Normal);
        press(&mut app, "d");
        assert_eq!(
            press(&mut app, "y"),
            vec![Effect::DeleteMuted {
                thread_id: "t2".into()
            }]
        );
    }

    fn rules(app: &mut App, mute: &[&str], priority: &[&str], allowed: &[&str]) {
        let v = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        app.set_rules(&v(mute), &v(priority), &v(allowed), vec![]);
    }

    #[test]
    fn aside_and_snoozed_threads_live_on_the_later_tab() {
        let mut app = inbox_app();
        let now = haste_core::time::now_secs();
        app.threads[0].triage.set_aside = true; // t1
        app.threads[1].triage.snoozed_until = Some(now + 3_600); // t2
        assert!(app.today_threads().is_empty(), "neither is in the inbox");
        press(&mut app, "2");
        assert_eq!(app.channel_threads(0).len(), 0, "nor in the channels");
        press(&mut app, "4");
        assert_eq!(app.view, View::Later);
        let ids: Vec<&str> = app
            .later_threads()
            .iter()
            .map(|t| t.thread.id.as_str())
            .collect();
        assert_eq!(ids, ["t2", "t1"], "the snoozed one wakes first");
        // g brings the one under the cursor back: aside off and snooze cleared
        assert_eq!(
            press(&mut app, "g"),
            vec![Effect::Triage {
                thread_id: "t2".into(),
                change: TriageChange::BringBack
            }]
        );
        // a snooze that has run out is back in the inbox
        app.threads[1].triage.snoozed_until = Some(now - 5);
        assert_eq!(app.today_threads().len(), 1);
        assert_eq!(app.later_threads().len(), 1);
    }

    #[test]
    fn priority_senders_rise_to_the_top_and_p_takes_one_thread_off() {
        let mut app = inbox_app();
        rules(&mut app, &[], &["^a@x\\.com$"], &[]);
        for t in &mut app.threads {
            t.sender = Some("a@x.com".into());
        }
        app.threads[0].thread.last_activity_at = 5; // t1 is older
        let first = |app: &App| app.today_threads()[0].thread.id.clone();
        assert!(app.is_priority(&app.threads[0].clone()));
        // both are priority by the rule: the newer one first, and p takes the cursor's off
        assert_eq!(first(&app), "t2");
        assert_eq!(
            press(&mut app, "p"),
            vec![Effect::Triage {
                thread_id: "t2".into(),
                change: TriageChange::Priority(-1)
            }]
        );
        app.threads[1].triage.priority = -1;
        assert!(!app.is_priority(&app.threads[1].clone()));
        assert_eq!(first(&app), "t1", "the one still on the list leads");
        // P toggles the sender on the list
        press(&mut app, "k");
        assert_eq!(
            press(&mut app, "P"),
            vec![Effect::PriorityRule {
                thread_id: "t1".into(),
                add: false,
                domain: false
            }]
        );
    }

    #[test]
    fn strangers_are_last_in_the_inbox_until_accepted_and_the_reader_refuses_actions() {
        let mut app = inbox_app();
        app.threads[1].sender_known = false;
        app.threads[1].sender = Some("new@shop.example".into());
        let ids = |app: &App| -> Vec<String> {
            app.today_threads()
                .iter()
                .map(|t| t.thread.id.clone())
                .collect()
        };
        assert_eq!(
            ids(&app),
            ["t1", "t2"],
            "the stranger is last, still in the inbox"
        );
        assert!(app.is_blocked(&app.threads[1].clone()));
        // priority or allowed senders are let through
        rules(&mut app, &[], &[], &["^new@shop\\.example$"]);
        assert!(!app.is_blocked(&app.threads[1].clone()));
        assert_eq!(ids(&app), ["t2", "t1"]);
        rules(&mut app, &[], &[], &[]);
        // b only works on a blocked thread
        assert!(press(&mut app, "b").is_empty(), "t1 is first and accepted");
        press(&mut app, "j");
        assert_eq!(
            press(&mut app, "b"),
            vec![Effect::Unblock {
                thread_id: "t2".into()
            }]
        );
        // opening a blocked thread does not mark it read, and actions are refused
        press(&mut app, "\n");
        let view = app.threads[1].clone();
        app.show_thread(view, vec![], None);
        assert!(app.reader.as_ref().unwrap().blocked);
        assert!(app.take_mark_read().is_none());
        assert!(press(&mut app, "r").is_empty());
        assert!(app.status.contains("blocked"), "{}", app.status);
        assert_eq!(
            press(&mut app, "b"),
            vec![Effect::Unblock {
                thread_id: "t2".into()
            }]
        );
    }

    #[test]
    fn u_toggles_read_and_opening_an_unread_thread_marks_it_read() {
        let mut app = inbox_app();
        app.threads[0].unread = true; // t1
        app.threads[1].unread = true; // t2, first in the list
        assert_eq!(
            press(&mut app, "u"),
            vec![Effect::SetRead {
                thread_id: "t2".into(),
                read: true
            }]
        );
        assert!(!app.threads[1].unread, "the list shows it read at once");
        assert_eq!(
            press(&mut app, "u"),
            vec![Effect::SetRead {
                thread_id: "t2".into(),
                read: false
            }]
        );
        assert!(app.threads[1].unread);
        // Ctrl-r refreshes now
        assert_eq!(app.handle_key(Key::Ctrl('r')), Some(Effect::Refresh));
        // opening t2 marks it read once
        let view = app.threads[1].clone();
        app.show_thread(view, vec![], None);
        assert_eq!(app.take_mark_read().as_deref(), Some("t2"));
        assert!(app.take_mark_read().is_none());
        assert!(!app.threads[1].unread);
    }

    fn reminder_view(id: &str, due: Option<i64>) -> ReminderView {
        ReminderView {
            reminder: haste_core::model::Reminder {
                id: id.into(),
                external_id: id.into(),
                title: format!("task {id}"),
                notes_snippet: String::new(),
                due_at: due,
                completed: false,
                list_name: "L".into(),
                remote_modified_at: 0,
                missing_since: None,
                version: 1,
            },
            tags: vec![],
            repeat: None,
        }
    }

    fn todo_app() -> App {
        let mut app = App::new();
        app.set_reminder_list(vec![
            reminder_view("a", None),
            reminder_view("b", None),
            reminder_view("c", None),
        ]);
        app.handle_key(Key::Tab); // to the TODO pane
        assert_eq!(app.today_focus, TodayFocus::Todo);
        app
    }

    #[test]
    fn e_marks_a_task_done_for_30_seconds_then_it_is_completed_and_removed() {
        let mut app = todo_app();
        let t0 = Instant::now();
        app.tick(t0);
        assert!(press(&mut app, "e").is_empty());
        assert_eq!(app.pending_done.len(), 1);
        assert_eq!(app.todos().len(), 3, "still listed, struck through");
        assert!(app.tick(t0 + Duration::from_secs(29)).is_none(), "not yet");
        let effect = app.tick(t0 + DONE_GRACE + Duration::from_secs(1));
        assert_eq!(
            effect,
            Some(Effect::CompleteReminders {
                ids: vec!["a".into()]
            })
        );
        assert_eq!(app.todos().len(), 2, "removed from the list at once");
        assert!(
            app.tick(t0 + Duration::from_secs(60)).is_none(),
            "only once"
        );
    }

    #[test]
    fn e_again_within_the_time_takes_it_back() {
        let mut app = todo_app();
        press(&mut app, "e");
        press(&mut app, "e");
        assert!(app.pending_done.is_empty());
        assert!(app
            .tick(Instant::now() + Duration::from_secs(120))
            .is_none());
    }

    #[test]
    fn space_selects_several_tasks_and_e_marks_them_all() {
        let mut app = todo_app();
        press(&mut app, " jj "); // a and c selected, cursor on c
        assert_eq!(app.todo_marked.len(), 2);
        press(&mut app, "e");
        assert_eq!(app.pending_done.len(), 2);
        assert!(app.todo_marked.is_empty());
        assert!(app.pending_done.contains_key("a") && app.pending_done.contains_key("c"));
    }

    #[test]
    fn quitting_hands_over_the_tasks_still_waiting() {
        let mut app = todo_app();
        press(&mut app, "e");
        let ids = app.take_pending_done();
        assert_eq!(ids, vec!["a".to_owned()]);
        assert!(app.pending_done.is_empty());
        assert_eq!(app.todos().len(), 2);
    }

    #[test]
    fn a_fresh_list_forgets_marks_of_tasks_that_are_gone() {
        let mut app = todo_app();
        press(&mut app, " e");
        app.set_reminder_list(vec![reminder_view("b", None)]);
        assert!(app.pending_done.is_empty() && app.todo_marked.is_empty());
    }

    fn message(id: &str, who: &str, text: &str, at: i64) -> Message {
        Message {
            id: id.into(),
            thread_id: "t2".into(),
            source: haste_core::model::Source::Gmail,
            external_id: id.into(),
            sender_person_id: None,
            sender_identifier: Some(who.into()),
            body_text: text.into(),
            sent_at: at,
            is_own: false,
        }
    }

    #[test]
    fn enter_opens_the_thread_on_today_and_inside_a_channel_but_not_on_the_todo_pane() {
        let mut app = inbox_app(); // cursor on t2
        assert_eq!(
            app.handle_key(Key::Enter),
            Some(Effect::OpenThread {
                thread_id: "t2".into()
            })
        );
        press(&mut app, "2");
        // on the channel list Enter opens the channel, not a thread
        assert!(app.handle_key(Key::Enter).is_none());
        assert!(app.channel_inner);
        assert_eq!(
            app.handle_key(Key::Enter),
            Some(Effect::OpenThread {
                thread_id: "t2".into()
            })
        );
        // the TODO pane has no thread to open
        press(&mut app, "1");
        app.handle_key(Key::Tab);
        assert!(app.handle_key(Key::Enter).is_none());
    }

    #[test]
    fn new_messages_reload_the_open_thread_and_keep_your_place() {
        let mut app = inbox_app();
        let view = app.threads[1].clone(); // thread t2
        app.show_thread(
            view,
            vec![
                message("m1", "a@x.com", "first", 10),
                message("m2", "b@x.com", "second", 20),
            ],
            None,
        );
        let other = Event::ThreadChanged {
            thread_id: "t1".into(),
        };
        assert_eq!(app.on_event(&other), Some(Effect::Refresh));
        let mine = Event::ThreadChanged {
            thread_id: "t2".into(),
        };
        assert_eq!(
            app.on_event(&mine),
            Some(Effect::ReloadThread {
                thread_id: "t2".into()
            })
        );
        // reading an older message: a new one arrives, the selection stays on the same message
        press(&mut app, "k");
        app.update_thread(vec![
            message("m3", "c@x.com", "third", 30),
            message("m1", "a@x.com", "first", 10),
            message("m2", "b@x.com", "second", 20),
        ]);
        let reader = app.reader.as_ref().unwrap();
        assert_eq!(reader.messages.len(), 3);
        assert_eq!(reader.messages[reader.selected].id, "m1");
        // reading the newest: the reader follows the conversation
        press(&mut app, "G");
        app.update_thread(vec![
            message("m1", "a@x.com", "first", 10),
            message("m2", "b@x.com", "second", 20),
            message("m3", "c@x.com", "third", 30),
            message("m4", "d@x.com", "fourth", 40),
        ]);
        let reader = app.reader.as_ref().unwrap();
        assert_eq!(reader.messages[reader.selected].id, "m4");
    }

    #[test]
    fn x_opens_and_folds_the_selected_message_and_reply_keeps_the_thread_in_view() {
        let mut app = inbox_app();
        let view = app.threads[1].clone();
        app.show_thread(
            view,
            vec![
                message("m1", "a@x.com", "first", 10),
                message("m2", "b@x.com", "second", 20),
            ],
            None,
        );
        press(&mut app, "k");
        press(&mut app, "x");
        assert!(app.reader.as_ref().unwrap().expanded.contains("m1"));
        press(&mut app, "x");
        assert!(app.reader.as_ref().unwrap().expanded.is_empty());
        // opening the reply form jumps to the newest message and keeps the reader
        app.open_reply_form("t2");
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.reader.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn the_attachment_menu_previews_downloads_one_or_all_into_a_chosen_folder() {
        let mut app = inbox_app();
        let view = app.threads[1].clone();
        app.show_thread(
            view,
            vec![
                message("m1", "a@x.com", "no files here", 10),
                message(
                    "m2",
                    "b@x.com",
                    "see attached\n[attachment: a.pdf (application/pdf, 100 bytes) #R1]\n[attachment: b.png (image/png, 50 bytes) #R2]",
                    20,
                ),
            ],
            None,
        );
        // the newest message has two files
        press(&mut app, "a");
        assert_eq!(app.mode, Mode::Attachments);
        assert_eq!(app.attach.as_ref().unwrap().files.len(), 2);
        // p asks for a preview of the selected file
        assert_eq!(
            app.handle_key(Key::Char('p')),
            Some(Effect::PreviewAttachment {
                message_id: "m2".into(),
                index: 0
            })
        );
        // D picks a folder for all of them; typing narrows, Enter chooses
        press(&mut app, "j");
        press(&mut app, "D");
        let dir = std::path::PathBuf::from("/Users/x/Downloads");
        {
            let (picker, indices) = app.attach.as_mut().unwrap().picker.as_mut().unwrap();
            assert_eq!(indices, &vec![0, 1]);
            *picker = editor::FilePicker::for_folders(vec![dir.clone()]);
        }
        assert_eq!(
            app.handle_key(Key::Enter),
            Some(Effect::SaveAttachments {
                files: vec![("m2".into(), 0), ("m2".into(), 1)],
                dir: dir.clone(),
            })
        );
        assert_eq!(app.mode, Mode::Read);
        // Enter on one file downloads just that one; Esc in the picker goes back to the list
        press(&mut app, "a");
        press(&mut app, "j");
        app.handle_key(Key::Enter);
        assert_eq!(
            app.attach.as_ref().unwrap().picker.as_ref().unwrap().1,
            vec![1]
        );
        app.handle_key(Key::Esc);
        assert!(app.attach.as_ref().unwrap().picker.is_none());
        app.handle_key(Key::Esc);
        assert_eq!(app.mode, Mode::Read);
        // a message without files says so
        press(&mut app, "k");
        press(&mut app, "a");
        assert_eq!(app.mode, Mode::Read);
        assert!(app.status.contains("no attachment"));
    }

    #[test]
    fn the_preview_scrolls_and_closes_back_to_the_menu() {
        let mut app = inbox_app();
        let view = app.threads[1].clone();
        app.show_thread(
            view,
            vec![message(
                "m1",
                "a@x.com",
                "[attachment: a.pdf (application/pdf, 1 bytes) #R]",
                10,
            )],
            None,
        );
        press(&mut app, "a");
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        app.show_preview("a.pdf", &text);
        assert_eq!(app.mode, Mode::Preview);
        press(&mut app, "j");
        assert_eq!(app.preview.as_ref().unwrap().scroll, 1);
        press(&mut app, " ");
        assert_eq!(app.preview.as_ref().unwrap().scroll, 1 + PREVIEW_PAGE);
        press(&mut app, "G");
        assert_eq!(app.preview.as_ref().unwrap().scroll, 99);
        app.handle_key(Key::Esc);
        assert_eq!(app.mode, Mode::Attachments);
    }

    #[test]
    fn the_reader_sorts_scrolls_replies_archives_and_closes() {
        let mut app = inbox_app();
        let view = app.threads[1].clone();
        app.show_thread(
            view,
            vec![
                message("m2", "b@x.com", "second", 20),
                message("m1", "a@x.com", "first", 10),
            ],
            None,
        );
        assert_eq!(app.mode, Mode::Read);
        let reader = app.reader.as_ref().unwrap();
        assert_eq!(reader.messages[0].id, "m1", "oldest first");
        assert_eq!(reader.selected, 1, "opens on the newest message");
        // j and k move between messages and stop at the ends
        press(&mut app, "j");
        assert_eq!(app.reader.as_ref().unwrap().selected, 1);
        press(&mut app, "k");
        assert_eq!(app.reader.as_ref().unwrap().selected, 0);
        press(&mut app, "k");
        assert_eq!(
            app.reader.as_ref().unwrap().selected,
            0,
            "never above the first"
        );
        press(&mut app, "G");
        assert_eq!(app.reader.as_ref().unwrap().selected, 1);
        press(&mut app, "g");
        assert_eq!(app.reader.as_ref().unwrap().selected, 0);
        // reply keeps the reader open
        assert_eq!(
            press(&mut app, "r"),
            vec![Effect::Reply {
                thread_id: "t2".into()
            }]
        );
        assert_eq!(app.mode, Mode::Read);
        // other thread keys do nothing in the reader
        for keys in ["g", "p", "x", "s", "t"] {
            assert!(press(&mut app, keys).is_empty(), "{keys}");
        }
        assert_eq!(app.mode, Mode::Read);
        // archive closes it and archives
        assert_eq!(
            press(&mut app, "e"),
            vec![Effect::Triage {
                thread_id: "t2".into(),
                change: TriageChange::Archive
            }]
        );
        assert_eq!(
            (app.mode.clone(), app.reader.is_none()),
            (Mode::Normal, true)
        );
        // Esc closes without doing anything
        let view = app.threads[0].clone();
        app.show_thread(view, vec![], None);
        assert!(app.handle_key(Key::Esc).is_none());
        assert_eq!(
            (app.mode.clone(), app.reader.is_none()),
            (Mode::Normal, true)
        );
    }

    #[test]
    fn a_reply_is_written_in_the_form_and_saved_as_a_draft_effect() {
        let mut app = inbox_app();
        app.open_reply_form("t1");
        assert_eq!(app.mode, Mode::Compose);
        assert!(app.handle_key(Key::Ctrl('s')).is_none(), "empty reply");
        // attaching works in a reply too: the picker opens, and a chosen file goes with the draft
        assert!(app.handle_key(Key::Ctrl('f')).is_none());
        assert!(app.compose.as_ref().unwrap().picker.is_some());
        app.compose.as_mut().unwrap().picker = None;
        app.compose
            .as_mut()
            .unwrap()
            .attachments
            .push(std::path::PathBuf::from("/a/report.pdf"));
        press(&mut app, "thanks");
        assert_eq!(
            app.handle_key(Key::Ctrl('s')),
            Some(Effect::SaveReply {
                thread_id: "t1".into(),
                body: "thanks".into(),
                attachments: vec![std::path::PathBuf::from("/a/report.pdf")],
            })
        );
    }

    #[test]
    fn an_archived_thread_leaves_the_list_at_once_and_returns_if_the_archive_fails() {
        let mut app = inbox_app(); // t2 then t1
        let before = app.threads.clone();
        app.begin_archive("t2");
        let ids =
            |a: &App| -> Vec<String> { a.threads.iter().map(|t| t.thread.id.clone()).collect() };
        assert_eq!(ids(&app), ["t1"]);
        // a reload while it is pending (the daemon still lists it) does not bring it back
        app.set_threads(before.clone());
        assert_eq!(ids(&app), ["t1"]);
        // success: it stays gone
        app.finish_archive("t2", Ok("archived"));
        assert_eq!(app.status, "archived");
        // failure: the next reload lists it again, with the reason shown
        app.begin_archive("t1");
        app.finish_archive("t1", Err("the mailbox refused".into()));
        assert!(app.status.contains("back in the list") && app.status.contains("refused"));
        app.set_threads(before);
        assert_eq!(ids(&app), ["t1", "t2"]);
    }

    #[test]
    fn plus_picks_a_reaction_for_a_chat_message_and_never_for_mail() {
        let mut app = App::new();
        let mail = mail_thread("t1", "Quarterly plan", &["a@x.com"], 10);
        let mut chat = mail_thread("t3", "Ana", &["a@x.com"], 30);
        chat.thread.source = haste_core::model::Source::Whatsapp;
        app.set_threads(vec![mail.clone(), chat.clone()]);
        // mail: refused, nothing to pick
        app.show_thread(mail, vec![message("m1", "a@x.com", "hi", 10)], None);
        assert!(press(&mut app, "+").is_empty());
        assert_eq!(app.mode, Mode::Read);
        assert!(app.status.contains("chats"), "{}", app.status);
        // chat: the picker opens and a digit sends that emoji for the selected message
        app.show_thread(chat, vec![message("m1", "a@x.com", "hi", 10)], None);
        assert!(press(&mut app, "+").is_empty());
        assert!(matches!(app.mode, Mode::React { .. }));
        assert_eq!(
            press(&mut app, "2"),
            vec![Effect::React {
                message_id: "m1".into(),
                key: "❤️".into()
            }]
        );
        assert_eq!(app.mode, Mode::Read);
        // an unknown digit and Esc send nothing
        press(&mut app, "+");
        assert!(press(&mut app, "9").is_empty());
        press(&mut app, "+");
        assert!(press(&mut app, "0").is_empty());
        assert_eq!(app.mode, Mode::Read);
    }

    #[test]
    fn a_reaction_shows_at_once_and_is_taken_back_when_it_fails() {
        let mut app = App::new();
        let mut chat = mail_thread("t3", "Ana", &["a@x.com"], 30);
        chat.thread.source = haste_core::model::Source::Whatsapp;
        app.set_threads(vec![chat.clone()]);
        app.show_thread(chat, vec![message("m1", "a@x.com", "hi", 10)], None);
        let body = |app: &App| app.reader.as_ref().unwrap().messages[0].body_text.clone();
        assert_eq!(body(&app), "hi");
        press(&mut app, "+");
        press(&mut app, "1");
        // the reaction is on the message before Beeper has answered
        assert!(body(&app).contains("👍"), "{}", body(&app));
        // it went through: it stays
        app.finish_reaction("m1", Ok(()));
        assert!(body(&app).contains("👍"));
        assert!(app.reactions_pending.is_empty());
        // another one that Beeper refuses: gone again, and the status says so
        press(&mut app, "+");
        press(&mut app, "2");
        assert!(body(&app).contains("❤️"));
        app.finish_reaction("m1", Err("Beeper refused".into()));
        assert!(!body(&app).contains("❤️"), "{}", body(&app));
        assert!(app.status.contains("taken back"), "{}", app.status);
        assert!(app.reactions_pending.is_empty());
    }

    #[test]
    fn a_preview_marks_pages_and_wraps_long_lines() {
        let mut app = inbox_app();
        let long = "word ".repeat(60);
        app.show_preview("f.pdf", &format!("one\n{long}\u{c}two"));
        let p = app.preview.as_ref().unwrap();
        assert!(p.lines.iter().any(|l| l == "── page 2 ──"));
        assert!(p.lines.iter().all(|l| inbox::width(l) <= PREVIEW_WIDTH));
        assert!(p.image.is_none());
    }

    #[test]
    fn a_batch_counts_as_one_in_the_reader_window() {
        let mut app = inbox_app();
        let view = app.threads[1].clone();
        let img = "[attachment: image.jpg (image/jpeg, 10 bytes)]";
        let mut messages = vec![message("t", "a@x.com", "text", 1)];
        for k in 0..8 {
            messages.push(message(&format!("i{k}"), "a@x.com", img, 100 + k));
        }
        app.show_thread(view, messages, None);
        let r = app.reader.as_mut().unwrap();
        r.shown = 1;
        assert_eq!(r.first_visible(), 1, "the whole batch is one item");
        r.shown = 2;
        assert_eq!(r.first_visible(), 0);
    }

    #[test]
    fn the_reader_shows_six_messages_and_o_shows_six_more() {
        let mut app = inbox_app();
        let view = app.threads[1].clone();
        let messages: Vec<Message> = (0..15)
            .map(|i| message(&format!("m{i:02}"), "a@x.com", "hi", i64::from(i) * 100))
            .collect();
        app.show_thread(view, messages, None);
        let r = app.reader.as_ref().unwrap();
        assert_eq!((r.first_visible(), r.selected), (9, 14));
        // moving up stops at the oldest shown message
        for _ in 0..20 {
            press(&mut app, "k");
        }
        assert_eq!(app.reader.as_ref().unwrap().selected, 9);
        press(&mut app, "o");
        assert_eq!(app.reader.as_ref().unwrap().first_visible(), 3);
        assert!(app.status.contains("12 of 15"), "{}", app.status);
        press(&mut app, "o");
        assert_eq!(app.reader.as_ref().unwrap().first_visible(), 0);
        // everything stored is shown: `o` asks the provider for older messages
        assert_eq!(
            press(&mut app, "o"),
            vec![Effect::LoadOlder {
                thread_id: app.reader.as_ref().unwrap().view.thread.id.clone()
            }]
        );
        press(&mut app, "g");
        assert_eq!(app.reader.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn reply_asks_only_when_several_people_and_reply_all_is_not_offered_a_send() {
        let mut app = inbox_app(); // t2 has three participants
        assert!(press(&mut app, "r").is_empty());
        assert!(matches!(app.mode, Mode::ReplyChoice { .. }));
        assert!(press(&mut app, "a").is_empty());
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.status.contains("reply-all"));
        press(&mut app, "r");
        assert_eq!(
            press(&mut app, "r"),
            vec![Effect::OpenAndReply {
                thread_id: "t2".into()
            }]
        );
        press(&mut app, "j"); // t1: one other person, straight to the thread and the reply
        assert_eq!(
            press(&mut app, "r"),
            vec![Effect::OpenAndReply {
                thread_id: "t1".into()
            }]
        );
    }

    #[test]
    fn filters_cycle_in_order_and_a_rejected_file_keeps_the_previous_set() {
        let mut app = inbox_app();
        let f = |name: &str, sender: &str| FilterView {
            name: name.into(),
            sender: vec![sender.into()],
            ..FilterView::default()
        };
        app.set_filters(vec![f("one", "a@x.com"), f("two", "c@x.com")], None);
        assert_eq!(app.today_threads().len(), 2);
        press(&mut app, "f");
        assert_eq!(app.active_filter().name, "two");
        assert_eq!(app.today_threads().len(), 1);
        press(&mut app, "f");
        assert_eq!(app.active_filter().name, "one", "wraps to the first");
        app.set_filters(vec![], Some("filter \"x\": unknown key \"y\"".into()));
        assert_eq!(app.filters.len(), 2, "previous valid set stays");
        assert!(app.filter_error.is_some());
    }

    #[test]
    fn channels_group_by_source_and_open_with_enter() {
        let mut app = inbox_app();
        press(&mut app, "2");
        assert_eq!(app.view, View::Channels);
        assert_eq!(
            app.channel_threads(0).len(),
            2,
            "gmail is the first channel"
        );
        assert_eq!(app.channel_threads(2).len(), 0);
        app.handle_key(Key::Enter);
        assert!(app.channel_inner);
        assert_eq!(
            press(&mut app, "e"),
            vec![Effect::Triage {
                thread_id: "t2".into(),
                change: TriageChange::Archive
            }]
        );
        app.handle_key(Key::Esc);
        assert!(!app.channel_inner);
    }

    #[test]
    fn todos_are_open_reminders_due_today_or_undated() {
        let mut app = App::new();
        let now = haste_core::time::now_secs();
        let r = |id: &str, due: Option<i64>, done: bool| ReminderView {
            reminder: haste_core::model::Reminder {
                id: id.into(),
                external_id: id.into(),
                title: id.into(),
                notes_snippet: String::new(),
                due_at: due,
                completed: done,
                list_name: "L".into(),
                remote_modified_at: 0,
                missing_since: None,
                version: 1,
            },
            tags: vec![],
            repeat: None,
        };
        app.set_reminder_list(vec![
            r("undated", None, false),
            r("today", Some(now), false),
            r("later", Some(now + 3 * 86_400), false),
            r("done", None, true),
        ]);
        let ids: Vec<&str> = app.todos().iter().map(|t| t.reminder.id.as_str()).collect();
        assert_eq!(ids, ["today", "undated"]);
    }

    #[test]
    fn n_is_not_a_thread_key_l_opens_labels_and_enter_edits_a_todo() {
        let mut app = App::new();
        app.set_threads(vec![mail_thread("t1", "Budget", &["a@x.org"], 100)]);
        // notes belong to TODO items now: n does nothing on a thread
        assert!(press(&mut app, "n").is_empty());
        assert_eq!(app.mode, Mode::Normal);
        // l: the daemon-side candidates are asked for, then picked
        assert_eq!(
            press(&mut app, "l"),
            vec![Effect::OpenLabels {
                thread_id: "t1".into()
            }]
        );
        app.show_labels("t1", vec!["haste".into()]);
        assert_eq!(app.mode, Mode::Label);
        assert_eq!(
            app.handle_key(Key::Enter),
            Some(Effect::LabelChange {
                thread_id: "t1".into(),
                label: "haste".into(),
                add: true
            })
        );
        app.handle_key(Key::Esc);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn c_on_the_todo_pane_opens_the_reminder_popup_with_nothing_filled_in() {
        let mut app = App::new();
        app.known_labels = vec!["haste".into()];
        app.today_focus = TodayFocus::Todo;
        press(&mut app, "c");
        assert_eq!(app.mode, Mode::RemindForm);
        let form = app.remind.as_ref().unwrap();
        assert!(form.title.is_empty() && form.suggestion.is_empty());
        assert!(form.thread_id.is_none());
        assert!(form.labels.iter().all(|(_, on)| !on), "no label is on");
        // without a title there is nothing to save
        assert!(app.handle_key(Key::Enter).is_none());
        assert_eq!(app.mode, Mode::RemindForm);
        press(&mut app, "pay rent every month");
        let Some(Effect::CreateReminder(task)) = app.handle_key(Key::Enter) else {
            panic!("expected a task")
        };
        assert_eq!(task.title, "pay rent every month");
        assert!(task.thread_id.is_none());
        assert_eq!(
            task.repeat.map(|r| r.freq),
            Some(haste_core::model::RepeatFreq::Month)
        );
        assert!(
            task.due.is_some(),
            "tomorrow morning unless the words say more"
        );
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn the_agenda_covers_seven_days_and_hides_cancelled_events() {
        let mut app = App::new();
        let now = haste_core::time::now_secs();
        let e = |id: &str, start: i64, status: &str| EventView {
            event: haste_core::model::Event {
                id: id.into(),
                external_id: id.into(),
                calendar: "c".into(),
                title: id.into(),
                start_at: start,
                end_at: start + 3600,
                all_day: false,
                status: status.into(),
                attendees: vec![],
                location: String::new(),
                description_snippet: String::new(),
                updated_at: 0,
                missing_since: None,
                version: 1,
            },
            tags: vec![],
        };
        app.set_events(vec![
            e("soon", now + 3600, "confirmed"),
            e("far", now + 9 * 86_400, "confirmed"),
            e("gone", now + 7200, "cancelled"),
        ]);
        let ids: Vec<&str> = app
            .agenda()
            .iter()
            .filter_map(|x| match x {
                AgendaItem::Event(e) => Some(e.event.id.as_str()),
                AgendaItem::Task { .. } => None,
            })
            .collect();
        assert_eq!(ids, ["soon"]);
    }

    #[test]
    fn j_k_g_big_g_move_selection() {
        let mut app = three(); // rows: A-header, p1, p2, B-header, p3, Blocked-header
        assert_eq!(app.rows().len(), 6);
        press(&mut app, "j");
        assert_eq!(app.approvals_selected, 1);
        press(&mut app, "G");
        assert_eq!(app.approvals_selected, 5);
        press(&mut app, "j"); // clamps at the end
        assert_eq!(app.approvals_selected, 5);
        press(&mut app, "g");
        assert_eq!(app.approvals_selected, 0);
        press(&mut app, "k");
        assert_eq!(app.approvals_selected, 0);
    }

    #[test]
    fn collapsing_a_group_hides_items_but_keeps_count() {
        let mut app = three();
        app.handle_key(Key::Enter); // on group header: toggle
        let rows = app.rows();
        assert_eq!(
            rows[0],
            Row::Group {
                principal: "agent-a".into(),
                count: 2,
                collapsed: true
            }
        );
        assert_eq!(rows.len(), 4);
        app.handle_key(Key::Enter);
        assert_eq!(app.rows().len(), 6);
    }

    #[test]
    fn slash_filters_and_escape_clears() {
        let mut app = three();
        press(&mut app, "/agent-b");
        app.handle_key(Key::Enter);
        assert_eq!(app.rows().len(), 3); // agent-b's header and item, plus the Blocked header
        press(&mut app, "/");
        app.handle_key(Key::Esc);
        assert_eq!(app.rows().len(), 6);
    }

    #[test]
    fn enter_on_item_opens_detail() {
        let mut app = three();
        press(&mut app, "j");
        app.handle_key(Key::Enter);
        assert_eq!(app.mode, Mode::Detail("p1".into()));
        app.handle_key(Key::Esc);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn batch_approve_needs_one_confirmation_showing_the_count() {
        let mut app = three();
        for _ in 0..3 {
            // mark p1, p2, p3 (skipping group headers)
            app.handle_key(Key::Char('j'));
            app.handle_key(Key::Char('x'));
            if app.approvals_selected == 2 {
                app.handle_key(Key::Char('j'));
            }
        }
        assert_eq!(app.marked.len(), 3);
        assert!(app.handle_key(Key::Char('A')).is_none());
        assert_eq!(
            app.confirm_prompt().unwrap(),
            "Approve 3 proposal(s)? [y/N]"
        );
        let effect = app.handle_key(Key::Char('y')).unwrap();
        assert!(
            matches!(effect, Effect::Approve { ids, acknowledge_stale: false } if ids.len() == 3)
        );
    }

    #[test]
    fn declining_the_batch_confirmation_does_nothing() {
        let mut app = three();
        press(&mut app, "jx");
        press(&mut app, "A");
        assert!(app.handle_key(Key::Char('n')).is_none());
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn stale_proposals_are_excluded_from_batch_with_a_reason() {
        let mut app = app_with(vec![
            proposal("p1", "agent-a", ProposalState::Pending, false),
            proposal("p2", "agent-a", ProposalState::Pending, true),
        ]);
        press(&mut app, "jx");
        press(&mut app, "jx");
        press(&mut app, "A");
        let Mode::ConfirmBatch(ids) = app.mode.clone() else {
            panic!("{:?}", app.mode)
        };
        assert_eq!(ids, vec!["p1".to_owned()]);
        assert!(app.status.contains("1 stale"));
    }

    #[test]
    fn stale_single_approval_requires_acknowledgement() {
        let mut app = app_with(vec![proposal(
            "p1",
            "agent-a",
            ProposalState::Pending,
            true,
        )]);
        press(&mut app, "j");
        assert!(app.handle_key(Key::Char('a')).is_none());
        assert!(matches!(app.mode, Mode::ConfirmStaleApprove(_)));
        let effect = app.handle_key(Key::Char('y')).unwrap();
        assert_eq!(
            effect,
            Effect::Approve {
                ids: vec!["p1".into()],
                acknowledge_stale: true
            }
        );
    }

    #[test]
    fn send_is_only_offered_for_approved_drafts_and_confirms_the_daemons_recipients() {
        let mut app = app_with(vec![
            proposal("p1", "agent-a", ProposalState::Pending, false),
            proposal("p2", "agent-a", ProposalState::Approved, false),
        ]);
        press(&mut app, "j"); // p1 pending
        assert!(press(&mut app, "s").is_empty());
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.status.contains("approved"));
        // on the approved draft `s` asks for a preview and changes nothing yet
        press(&mut app, "j");
        assert_eq!(
            app.handle_key(Key::Char('s')),
            Some(Effect::PreviewSend { id: "p2".into() })
        );
        assert_eq!(app.mode, Mode::Normal);
        // the preview names the resolved recipient, the subject, the source and the budget
        app.show_send_preview(
            "p2",
            SendPreviewView {
                source: "gmail".into(),
                recipients: vec!["ana@x.org".into()],
                subject: "Re: plan".into(),
                body_bytes: 10,
                attachments: Vec::new(),
                stale: false,
                sends_remaining: 7,
                recipients_digest: "digest-1".into(),
            },
        );
        // a normal send is a countdown, not a y prompt
        let prompt = app.confirm_prompt().unwrap();
        for part in [
            "SENDING to ana@x.org",
            "gmail",
            "Re: plan",
            "7 left",
            "in 5s",
            "any key cancels",
        ] {
            assert!(prompt.contains(part), "{prompt}");
        }
        let start = app.now;
        // nothing is sent before the delay has run out
        assert_eq!(app.tick(start + Duration::from_secs(4)), None);
        assert!(app.confirm_prompt().unwrap().contains("in 1s"));
        // any key cancels, and nothing is sent afterwards
        assert!(press(&mut app, "n").is_empty());
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.status.contains("cancelled"));
        assert_eq!(app.tick(start + SEND_DELAY + Duration::from_secs(1)), None);
        // let it run: after the delay the send goes out once, with the displayed digest
        app.show_send_preview("p2", preview(false));
        let start = app.now;
        assert_eq!(
            app.tick(start + SEND_DELAY - Duration::from_millis(1)),
            None
        );
        assert_eq!(
            app.tick(start + SEND_DELAY),
            Some(Effect::Send {
                id: "p2".into(),
                acknowledge_stale: false,
                recipients_digest: "digest-1".into()
            })
        );
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(
            app.tick(start + SEND_DELAY * 2),
            None,
            "the send is returned once"
        );
        // a stale thread still asks for an explicit y, and acknowledges the staleness
        app.show_send_preview("p2", preview(true));
        assert!(app.confirm_prompt().unwrap().contains("SEND to ana@x.org"));
        assert_eq!(
            app.tick(app.now + SEND_DELAY * 3),
            None,
            "no timer on a stale send"
        );
        assert_eq!(
            app.handle_key(Key::Char('y')),
            Some(Effect::Send {
                id: "p2".into(),
                acknowledge_stale: true,
                recipients_digest: "digest-1".into()
            })
        );
    }

    #[test]
    fn a_send_whose_recipients_changed_is_never_sent_on_a_timer() {
        let mut app = app_with(vec![proposal(
            "p2",
            "agent-a",
            ProposalState::Approved,
            false,
        )]);
        app.show_confirmed_send_preview("p2", preview(false));
        assert!(app.confirm_prompt().unwrap().contains("[y/N]"));
        assert_eq!(app.tick(app.now + SEND_DELAY * 3), None);
        assert!(matches!(app.mode, Mode::ConfirmSend { .. }));
    }

    fn preview(stale: bool) -> SendPreviewView {
        SendPreviewView {
            source: "gmail".into(),
            recipients: vec!["ana@x.org".into()],
            subject: "Re: plan".into(),
            body_bytes: 10,
            attachments: Vec::new(),
            stale,
            sends_remaining: 7,
            recipients_digest: "digest-1".into(),
        }
    }

    #[test]
    fn a_chat_send_confirmation_names_the_chat_and_network_and_has_no_subject() {
        let mut app = app_with(vec![proposal(
            "p2",
            "agent-a",
            ProposalState::Approved,
            false,
        )]);
        press(&mut app, "j");
        app.show_send_preview(
            "p2",
            SendPreviewView {
                source: "whatsapp".into(),
                recipients: vec!["Family chat".into()],
                subject: String::new(),
                body_bytes: 12,
                attachments: Vec::new(),
                stale: false,
                sends_remaining: 4,
                recipients_digest: "digest-chat".into(),
            },
        );
        let prompt = app.confirm_prompt().unwrap();
        assert!(
            prompt.contains("SENDING to Family chat via whatsapp"),
            "{prompt}"
        );
        assert!(!prompt.contains("subject"), "{prompt}");
        assert!(prompt.contains("4 left"), "{prompt}");
    }

    #[test]
    fn a_proposal_that_needs_a_check_is_settled_only_after_a_confirmation_each_way() {
        let mut app = app_with(vec![
            reminder_proposal("e1", ProposalState::Executing),
            proposal("a1", "agent-a", ProposalState::Approved, false),
        ]);
        press(&mut app, "j"); // e1
        assert!(press(&mut app, "m").is_empty());
        assert!(app.confirm_prompt().unwrap().contains("IS in the mailbox"));
        assert_eq!(
            press(&mut app, "y"),
            vec![Effect::Resolve {
                id: "e1".into(),
                done: true
            }]
        );
        press(&mut app, "b");
        assert!(app
            .confirm_prompt()
            .unwrap()
            .contains("NOTHING was delivered"));
        assert_eq!(
            press(&mut app, "y"),
            vec![Effect::Resolve {
                id: "e1".into(),
                done: false
            }]
        );
        // anything but y cancels, and an ordinary proposal cannot be settled this way
        press(&mut app, "m");
        assert!(press(&mut app, "n").is_empty());
        assert_eq!(app.mode, Mode::Normal);
        press(&mut app, "j");
        assert!(press(&mut app, "m").is_empty());
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.status.contains("NEEDS CHECK"));
    }

    #[test]
    fn reject_applies_to_pending_only() {
        let mut app = app_with(vec![
            proposal("p1", "agent-a", ProposalState::Pending, false),
            proposal("p2", "agent-a", ProposalState::Approved, false),
        ]);
        press(&mut app, "j");
        assert_eq!(
            app.handle_key(Key::Char('r')),
            Some(Effect::Reject { id: "p1".into() })
        );
        press(&mut app, "j");
        assert!(app.handle_key(Key::Char('r')).is_none());
    }

    #[test]
    fn proposal_events_trigger_refresh_and_notices_are_kept() {
        let mut app = three();
        let e = app.on_event(&Event::ProposalChanged {
            proposal_id: "p9".into(),
            state: ProposalState::Pending,
        });
        assert_eq!(e, Some(Effect::Refresh));
        assert_eq!(
            app.on_event(&Event::Notice {
                message: "connector stopped".into()
            }),
            Some(Effect::Refresh)
        );
        assert_eq!(app.notices, vec!["connector stopped".to_owned()]);
        // an agent finishing or being blocked in herdr is not announced; a herdr problem is
        for quiet in [
            "[herdr:local] claude in 'website' is done",
            "[herdr:m1] codex in 'api' is blocked (+2 suppressed)",
        ] {
            assert_eq!(
                app.on_event(&Event::Notice {
                    message: quiet.into()
                }),
                None
            );
        }
        assert_eq!(app.notices.len(), 1);
        assert!(app
            .on_event(&Event::Notice {
                message: "[herdr:local] protocol mismatch: expected 22, saw 23".into()
            })
            .is_some());
    }

    fn agent(
        machine: &str,
        label: &str,
        pane: &str,
        status: AgentStatus,
        since: i64,
    ) -> AgentStatusRecord {
        AgentStatusRecord {
            machine: machine.into(),
            workspace_id: "w1".into(),
            workspace_label: label.into(),
            pane_id: pane.into(),
            agent: Some("claude".into()),
            status,
            status_since: since,
        }
    }

    fn machine(name: &str, state: MachineState, agents: Vec<AgentStatusRecord>) -> MachineView {
        MachineView {
            machine: name.into(),
            state,
            agents,
        }
    }

    fn with_project(mut p: Proposal, project: Option<&str>) -> Proposal {
        p.project = project.map(str::to_owned);
        p
    }

    #[test]
    fn blocked_agents_appear_in_their_own_group_oldest_first_and_vanish_with_the_state() {
        let mut app = three();
        // an empty group still shows its header with a zero count
        assert_eq!(
            app.rows().last(),
            Some(&Row::BlockedGroup {
                count: 0,
                collapsed: false
            })
        );
        app.set_machines(vec![
            machine(
                "local",
                MachineState::Connected,
                vec![agent("local", "haste", "w1:p1", AgentStatus::Blocked, 200)],
            ),
            machine(
                "m1",
                MachineState::Connected,
                vec![
                    agent("m1", "notes", "w1:p1", AgentStatus::Blocked, 100),
                    agent("m1", "notes", "w1:p2", AgentStatus::Working, 50),
                ],
            ),
        ]);
        let blocked = app.blocked();
        assert_eq!(
            blocked
                .iter()
                .map(|a| a.machine.as_str())
                .collect::<Vec<_>>(),
            ["m1", "local"],
            "oldest first"
        );
        let rows = app.rows();
        assert_eq!(
            rows[rows.len() - 3],
            Row::BlockedGroup {
                count: 2,
                collapsed: false
            }
        );
        assert_eq!(&rows[rows.len() - 2..], [Row::Blocked(0), Row::Blocked(1)]);
        // the agent recovers: the group empties without any user action
        app.set_machines(vec![machine(
            "m1",
            MachineState::Connected,
            vec![agent("m1", "notes", "w1:p1", AgentStatus::Working, 300)],
        )]);
        assert_eq!(
            app.rows().last(),
            Some(&Row::BlockedGroup {
                count: 0,
                collapsed: false
            })
        );
    }

    #[test]
    fn an_unreachable_machine_contributes_no_blocked_agents() {
        let mut app = three();
        app.set_machines(vec![machine(
            "m1",
            MachineState::Unreachable,
            vec![agent("m1", "x", "w1:p1", AgentStatus::Blocked, 1)],
        )]);
        assert!(app.blocked().is_empty());
    }

    #[test]
    fn the_blocked_group_is_read_only_and_never_part_of_a_batch() {
        let mut app = three();
        app.set_machines(vec![machine(
            "m1",
            MachineState::Connected,
            vec![agent("m1", "notes", "w1:p1", AgentStatus::Blocked, 1)],
        )]);
        // go to the blocked entry (last row) and try every action key
        press(&mut app, "G");
        assert!(matches!(
            app.rows()[app.approvals_selected],
            Row::Blocked(0)
        ));
        for keys in ["a", "A", "x", "s", "r", " "] {
            assert!(
                press(&mut app, keys).is_empty(),
                "{keys:?} must not act on a blocked agent"
            );
        }
        assert!(app.marked.is_empty(), "a blocked agent cannot be marked");
        assert_eq!(app.mode, Mode::Normal);
        // Enter only says where it is
        app.handle_key(Key::Enter);
        assert!(
            app.status.contains("m1")
                && app.status.contains("notes")
                && app.status.contains("read-only"),
            "{}",
            app.status
        );
        // a batch of one proposal plus the blocked entry confirms exactly one
        press(&mut app, "g");
        press(&mut app, "jx");
        press(&mut app, "G");
        press(&mut app, "x");
        press(&mut app, "A");
        assert_eq!(
            app.confirm_prompt().as_deref(),
            Some("Approve 1 proposal(s)? [y/N]")
        );
    }

    #[test]
    fn the_blocked_group_collapses_like_the_others() {
        let mut app = three();
        app.set_machines(vec![machine(
            "m1",
            MachineState::Connected,
            vec![agent("m1", "n", "w1:p1", AgentStatus::Blocked, 1)],
        )]);
        press(&mut app, "G");
        press(&mut app, "k"); // the Blocked header
        app.handle_key(Key::Enter);
        assert_eq!(
            app.rows().last(),
            Some(&Row::BlockedGroup {
                count: 1,
                collapsed: true
            })
        );
    }

    #[test]
    fn proposals_group_by_project_with_a_no_project_group() {
        let mut app = app_with(vec![
            with_project(
                proposal("p1", "agent-a", ProposalState::Pending, false),
                Some("haste"),
            ),
            with_project(
                proposal("p2", "agent-b", ProposalState::Pending, false),
                Some("haste"),
            ),
            with_project(
                proposal("p3", "agent-a", ProposalState::Pending, false),
                Some("notes"),
            ),
            proposal("p4", "agent-a", ProposalState::Pending, false),
        ]);
        let by_agent: Vec<String> = app
            .rows()
            .iter()
            .filter_map(|r| match r {
                Row::Group { principal, .. } => Some(principal.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(by_agent, ["agent-a", "agent-b"]);
        press(&mut app, "P");
        assert_eq!(app.group_by, GroupBy::Project);
        let groups: Vec<(String, usize)> = app
            .rows()
            .iter()
            .filter_map(|r| match r {
                Row::Group {
                    principal, count, ..
                } => Some((principal.clone(), *count)),
                _ => None,
            })
            .collect();
        assert_eq!(
            groups,
            [
                ("haste".into(), 2),
                ("no project".into(), 1),
                ("notes".into(), 1)
            ]
        );
        press(&mut app, "P");
        assert_eq!(app.group_by, GroupBy::Agent);
    }

    #[test]
    fn collapsing_in_one_grouping_mode_does_not_collapse_the_same_name_in_the_other() {
        let mut app = app_with(vec![with_project(
            proposal("p1", "haste", ProposalState::Pending, false),
            Some("haste"),
        )]);
        app.handle_key(Key::Enter); // collapse the agent group named "haste"
        assert!(matches!(
            app.rows()[0],
            Row::Group {
                collapsed: true,
                ..
            }
        ));
        press(&mut app, "P"); // the project group named "haste" is separate
        assert!(matches!(
            app.rows()[0],
            Row::Group {
                collapsed: false,
                ..
            }
        ));
    }

    #[test]
    fn the_filter_matches_a_proposals_project() {
        let mut app = app_with(vec![
            with_project(
                proposal("p1", "agent-a", ProposalState::Pending, false),
                Some("haste"),
            ),
            with_project(
                proposal("p2", "agent-a", ProposalState::Pending, false),
                Some("notes"),
            ),
        ]);
        press(&mut app, "/haste");
        app.handle_key(Key::Enter);
        let items = app
            .rows()
            .iter()
            .filter(|r| matches!(r, Row::Item(_)))
            .count();
        assert_eq!(items, 1);
    }

    #[test]
    fn tab_switches_the_panes_on_today_and_later_and_cycles_the_other_tabs() {
        let mut app = App::new();
        // on Today, Tab switches between the Incoming and TODO panes
        app.handle_key(Key::Tab);
        assert_eq!((app.view, app.today_focus), (View::Today, TodayFocus::Todo));
        app.handle_key(Key::Tab);
        assert_eq!(
            (app.view, app.today_focus),
            (View::Today, TodayFocus::Inbox)
        );
        // on the other tabs it cycles the tabs
        press(&mut app, "2");
        for expected in [View::Calendar, View::Later] {
            app.handle_key(Key::Tab);
            assert_eq!(app.view, expected);
        }
        // Later has two panes like Today: Tab switches between them
        app.handle_key(Key::Tab);
        assert_eq!((app.view, app.later_focus), (View::Later, TodayFocus::Todo));
        app.handle_key(Key::Tab);
        assert_eq!(
            (app.view, app.later_focus),
            (View::Later, TodayFocus::Inbox)
        );
    }

    fn connectors() -> Vec<ConnectorView> {
        vec![
            ConnectorView {
                source: "gmail".into(),
                engaged: true,
                registered: true,
            },
            ConnectorView {
                source: "icloud".into(),
                engaged: false,
                registered: true,
            },
        ]
    }

    fn connectors_app() -> App {
        let mut app = approvals_app();
        app.set_connectors(connectors());
        press(&mut app, "6");
        assert_eq!(app.view, View::Connectors);
        app
    }

    #[test]
    fn connectors_panel_navigates_with_vi_keys() {
        let mut app = connectors_app();
        press(&mut app, "j");
        assert_eq!(app.connectors_selected, 1);
        press(&mut app, "j");
        assert_eq!(app.connectors_selected, 1);
        press(&mut app, "g");
        assert_eq!(app.connectors_selected, 0);
        press(&mut app, "G");
        assert_eq!(app.connectors_selected, 1);
    }

    #[test]
    fn release_asks_for_confirmation_and_only_y_sends_it() {
        let mut app = connectors_app();
        assert!(app.handle_key(Key::Char('R')).is_none());
        assert!(app.confirm_prompt().unwrap().starts_with("Release gmail?"));
        assert_eq!(
            app.handle_key(Key::Char('y')),
            Some(Effect::ReleaseConnector {
                source: "gmail".into()
            })
        );
        app.handle_key(Key::Char('R'));
        assert!(app.handle_key(Key::Enter).is_none());
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.status, "cancelled");
    }

    #[test]
    fn release_is_not_offered_for_a_connector_that_is_not_engaged() {
        let mut app = connectors_app();
        press(&mut app, "jR");
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.status.contains("not engaged"));
    }

    #[test]
    fn engage_is_direct_and_only_for_running_connectors() {
        let mut app = connectors_app();
        assert!(app.handle_key(Key::Char('E')).is_none());
        press(&mut app, "j");
        assert_eq!(
            app.handle_key(Key::Char('E')),
            Some(Effect::EngageConnector {
                source: "icloud".into()
            })
        );
    }

    #[test]
    fn connector_keys_do_nothing_in_other_views() {
        let mut app = three();
        app.set_connectors(connectors());
        assert!(press(&mut app, "RE").is_empty());
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn closed_proposals_drop_out_of_the_list_and_marks() {
        let mut app = three();
        press(&mut app, "jx");
        app.set_proposals(vec![proposal("p1", "agent-a", ProposalState::Sent, false)]);
        assert!(app.proposals.is_empty());
        assert!(app.marked.is_empty());
    }
}
