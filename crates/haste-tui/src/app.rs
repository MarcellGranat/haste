//! Pure TUI state and key handling. No terminal access, so everything here is unit-testable.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Tab 1: inbox of all channels plus today's TODOs.
    Today,
    /// Tab 2: the inbox by channel.
    Channels,
    /// Tab 3: the next days of the calendar.
    Calendar,
    /// Secondary screens, opened with 4, 5 and 6.
    Approvals,
    Connectors,
    Agents,
}

impl View {
    /// The three main tabs, in order.
    pub const TABS: [Self; 3] = [Self::Today, Self::Channels, Self::Calendar];

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
    Priority(i32),
    /// Snooze until this unix second.
    SnoozeUntil(i64),
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
    /// Start at the newest message; cleared once drawn.
    pub to_latest: Cell<bool>,
}

/// Lines a page key moves in the reader.
pub const READER_PAGE: usize = 12;

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
    /// Propose a reminder about a thread (the human approves it in Approvals).
    ToReminder {
        thread_id: String,
        title: String,
        due: Option<i64>,
    },
    /// Change a thread's triage state (local to haste; no mailbox or chat network is touched).
    Triage {
        thread_id: String,
        change: TriageChange,
    },
    /// Fetch a thread's messages to read it.
    OpenThread {
        thread_id: String,
    },
    /// Hide a thread's sender from the TUI (the local `.haste_ignore` list).
    Mute {
        thread_id: String,
    },
    /// Complete these reminders in Reminders (propose, approve and run, as the human).
    CompleteReminders {
        ids: Vec<String>,
    },
    /// Open the editor for a reply to a thread; the text becomes a draft proposal (never sent).
    Reply {
        thread_id: String,
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
    /// Typing a filter after `/`.
    Filter,
    Detail(String),
    ConfirmBatch(Vec<String>),
    ConfirmStaleApprove(String),
    ConfirmRelease(String),
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
    /// Typing a snooze (`3h`, `2d`).
    Snooze {
        thread_id: String,
        input: String,
    },
    /// Choosing when to be reminded about a thread: one key for a quick pick, or `c` to type a
    /// date (`custom` holds the text being typed).
    Remind {
        thread_id: String,
        title: String,
        custom: Option<String>,
    },
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

/// A row of the Agents panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRow {
    Machine { name: String, state: MachineState },
    Workspace(String),
    Agent(AgentStatusRecord),
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
    /// Muted senders (`.haste_ignore`), applied to the Today and Channels lists only.
    pub ignore: IgnoreList,
    pub ignore_errors: Vec<String>,
    /// Tasks selected with Space in the TODO pane.
    pub todo_marked: BTreeSet<String>,
    /// Tasks marked done: shown struck through until this moment, then completed and removed.
    pub pending_done: BTreeMap<String, Instant>,
    /// Tasks being completed right now: hidden until the next list arrives.
    completing: BTreeSet<String>,
    pub today_focus: TodayFocus,
    pub todo_selected: usize,
    /// Offset of the machine's local time from UTC (seconds), refreshed by the loop.
    pub tz_offset: i64,
    tz_checked: Option<Instant>,
    /// First visible rows of the scrolling lists (set while drawing).
    pub inbox_offset: Cell<usize>,
    pub todo_offset: Cell<usize>,
    pub channel_offset: Cell<usize>,
    pub channels_selected: usize,
    /// Inside a channel's thread list (Channels tab).
    pub channel_inner: bool,
    pub channel_thread_selected: usize,
    pub approvals_selected: usize,
    pub connectors: Vec<ConnectorView>,
    /// Reminders permission and counts (no titles); `None` until first read.
    pub reminders: Option<RemindersStatusView>,
    pub connectors_selected: usize,
    pub machines: Vec<MachineView>,
    pub agents_selected: usize,
    /// Upcoming calendar events, soonest first (read-only).
    pub events: Vec<EventView>,
    pub calendar_selected: usize,
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

/// One-line description of what a proposal would do.
#[must_use]
pub fn summary(p: &Proposal) -> String {
    match &p.payload {
        Payload::Draft { to, body } => {
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
            ignore: IgnoreList::default(),
            ignore_errors: Vec::new(),
            todo_marked: BTreeSet::new(),
            pending_done: BTreeMap::new(),
            completing: BTreeSet::new(),
            today_focus: TodayFocus::Inbox,
            todo_selected: 0,
            tz_offset: 0,
            tz_checked: None,
            inbox_offset: Cell::new(0),
            todo_offset: Cell::new(0),
            channel_offset: Cell::new(0),
            channels_selected: 0,
            channel_inner: false,
            channel_thread_selected: 0,
            approvals_selected: 0,
            connectors: Vec::new(),
            reminders: None,
            connectors_selected: 0,
            machines: Vec::new(),
            agents_selected: 0,
            events: Vec::new(),
            calendar_selected: 0,
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
        self.clamp();
    }

    /// Daemon change events: proposals refresh the list, notices are shown to the human.
    pub fn on_event(&mut self, event: &Event) -> Option<Effect> {
        match event {
            Event::ProposalChanged { .. } | Event::ThreadChanged { .. } => Some(Effect::Refresh),
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

    /// Rows of the Agents panel: each machine with its state, and while it is connected its
    /// workspaces (by label) with their agents.
    #[must_use]
    pub fn agent_rows(&self) -> Vec<AgentRow> {
        let mut rows = Vec::new();
        for m in &self.machines {
            rows.push(AgentRow::Machine {
                name: m.machine.clone(),
                state: m.state.clone(),
            });
            if m.state != MachineState::Connected {
                continue; // never show stale agents as current
            }
            let mut labels: Vec<&str> = m
                .agents
                .iter()
                .map(|a| a.workspace_label.as_str())
                .collect();
            labels.sort_unstable();
            labels.dedup();
            for label in labels {
                rows.push(AgentRow::Workspace(label.to_owned()));
                let mut agents: Vec<&AgentStatusRecord> = m
                    .agents
                    .iter()
                    .filter(|a| a.workspace_label == label)
                    .collect();
                agents.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
                rows.extend(agents.into_iter().map(|a| AgentRow::Agent(a.clone())));
            }
        }
        rows
    }

    /// Threads passing the active filter, best first: priority, then newest activity.
    fn filtered_threads(&self, include_aside: bool) -> Vec<&ThreadView> {
        let filter = self.active_filter();
        let mut out: Vec<&ThreadView> = self
            .threads
            .iter()
            .filter(|t| !t.triage.archived && (include_aside || !t.triage.set_aside))
            .filter(|t| inbox::matches(filter, t))
            .filter(|t| !self.ignore.is_muted(&inbox::senders(t)))
            .collect();
        out.sort_by(|a, b| {
            (b.triage.priority, b.thread.last_activity_at)
                .cmp(&(a.triage.priority, a.thread.last_activity_at))
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

    /// TODOs for today: open reminders due by the end of today (overdue included) or undated.
    #[must_use]
    pub fn todos(&self) -> Vec<&ReminderView> {
        let end = inbox::day_start(haste_core::time::now_secs(), self.tz_offset) + 86_400;
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

    /// Threads of channel `i` under the active filter (set-aside ones included, marked).
    #[must_use]
    pub fn channel_threads(&self, i: usize) -> Vec<&ThreadView> {
        let Some(name) = inbox::CHANNELS.get(i) else {
            return Vec::new();
        };
        self.filtered_threads(true)
            .into_iter()
            .filter(|t| t.thread.source.as_name() == *name)
            .collect()
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
            _ => None,
        }
    }

    /// Calendar rows for the next [`inbox::AGENDA_DAYS`] days, soonest first.
    #[must_use]
    pub fn agenda(&self) -> Vec<&EventView> {
        let now = haste_core::time::now_secs();
        let until = inbox::day_start(now, self.tz_offset) + inbox::AGENDA_DAYS * 86_400;
        let mut out: Vec<&EventView> = self
            .events
            .iter()
            .filter(|e| {
                e.event.status != "cancelled" && e.event.end_at >= now && e.event.start_at < until
            })
            .collect();
        out.sort_by_key(|e| e.event.start_at);
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
        self.agents_selected = self
            .agents_selected
            .min(self.agent_rows().len().saturating_sub(1));
        self.calendar_selected = self
            .calendar_selected
            .min(self.agenda().len().saturating_sub(1));
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
            View::Approvals => self.rows().len(),
            View::Connectors => self.connectors.len(),
            View::Agents => self.agent_rows().len(),
        }
    }

    fn selection_mut(&mut self) -> &mut usize {
        match self.view {
            View::Today if self.today_focus == TodayFocus::Todo => &mut self.todo_selected,
            View::Today => &mut self.today_selected,
            View::Channels if self.channel_inner => &mut self.channel_thread_selected,
            View::Channels => &mut self.channels_selected,
            View::Calendar => &mut self.calendar_selected,
            View::Approvals => &mut self.approvals_selected,
            View::Connectors => &mut self.connectors_selected,
            View::Agents => &mut self.agents_selected,
        }
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
            Mode::Snooze { thread_id, input } => self.key_snooze(key, thread_id, input),
            Mode::Remind {
                thread_id,
                title,
                custom,
            } => self.key_remind(key, thread_id, title, custom),
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
            || (self.view == View::Channels && self.channel_inner);
        match key {
            Key::Char('q') => return Some(Effect::Quit),
            Key::Tab if self.view == View::Today => {
                self.today_focus = match self.today_focus {
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
            Key::Char('4') => self.view = View::Approvals,
            Key::Char('5') => self.view = View::Connectors,
            Key::Char('6') => self.view = View::Agents,
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
            Key::Enter if thread_view => return self.open_selected(),
            Key::Char('f') if matches!(self.view, View::Today | View::Channels) => {
                self.cycle_filter();
            }
            Key::Char('e') if thread_view => return self.triage_archive(),
            Key::Char('x') if thread_view => return self.mute_selected(),
            Key::Char(' ') if self.on_todos() => self.toggle_todo_mark(),
            Key::Char('e') if self.on_todos() => self.mark_done(),
            Key::Char('g') if thread_view => return self.triage_aside(),
            Key::Char('p') if thread_view => return self.triage_priority(),
            Key::Char('s') if thread_view => self.start_snooze(),
            Key::Char('t') if thread_view => self.start_remind(),
            Key::Char('r') if thread_view => return self.start_reply(),
            Key::Char('0') => self.jump(false),
            Key::Char('P') if self.view == View::Approvals => self.toggle_group_by(),
            Key::Char('R') if self.view == View::Connectors => self.start_release(),
            Key::Char('E') if self.view == View::Connectors => return self.engage_selected(),
            Key::Char('j') | Key::Down => self.move_selection(1),
            Key::Char('k') | Key::Up => self.move_selection(-1),
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
            subject: preview.subject,
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
            subject: preview.subject,
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
    pub fn show_thread(&mut self, view: ThreadView, mut messages: Vec<Message>) {
        messages.sort_by_key(|m| m.sent_at);
        self.reader = Some(Reader {
            view,
            messages,
            scroll: Cell::new(0),
            to_latest: Cell::new(true),
        });
        self.mode = Mode::Read;
    }

    fn close_reader(&mut self) {
        self.reader = None;
        self.mode = Mode::Normal;
    }

    /// Keys while a thread is open: scroll, reply, archive, close.
    fn key_read(&mut self, key: Key) -> Option<Effect> {
        let Some(reader) = &self.reader else {
            self.mode = Mode::Normal;
            return None;
        };
        let thread_id = reader.view.thread.id.clone();
        let at = reader.scroll.get();
        let to = |v: usize| reader.scroll.set(v);
        match key {
            Key::Esc | Key::Enter | Key::Char('q' | 'h') => self.close_reader(),
            Key::Char('j') | Key::Down => to(at.saturating_add(1)),
            Key::Char('k') | Key::Up => to(at.saturating_sub(1)),
            Key::Char(' ' | 'd') => to(at.saturating_add(READER_PAGE)),
            Key::Char('b' | 'u') => to(at.saturating_sub(READER_PAGE)),
            Key::Char('g' | '0') => {
                reader.to_latest.set(false);
                to(0);
            }
            Key::Char('G') => to(usize::MAX),
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

    fn on_todos(&self) -> bool {
        self.view == View::Today && self.today_focus == TodayFocus::Todo
    }

    fn mute_selected(&mut self) -> Option<Effect> {
        let (thread_id, _) = self.thread_under_cursor()?;
        Some(Effect::Mute { thread_id })
    }

    fn toggle_todo_mark(&mut self) {
        let Some(id) = self
            .todos()
            .get(self.todo_selected)
            .map(|r| r.reminder.id.clone())
        else {
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
        let under_cursor = self
            .todos()
            .get(self.todo_selected)
            .map(|r| r.reminder.id.clone());
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

    fn triage_aside(&mut self) -> Option<Effect> {
        let t = self.selected_thread()?;
        Some(Effect::Triage {
            thread_id: t.thread.id.clone(),
            change: TriageChange::SetAside(!t.triage.set_aside),
        })
    }

    fn triage_priority(&mut self) -> Option<Effect> {
        let t = self.selected_thread()?;
        let next = if t.triage.priority > 0 {
            0
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

    fn start_remind(&mut self) {
        if let Some((thread_id, title)) = self.thread_under_cursor() {
            self.mode = Mode::Remind {
                thread_id,
                title,
                custom: None,
            };
        }
    }

    /// The due time `Enter` accepts in the reminder prompt, from the thread's subject.
    fn suggested_due(&self, title: &str) -> i64 {
        inbox::suggest_due(title, haste_core::time::now_secs(), self.tz_offset)
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
            Some(Effect::Reply { thread_id })
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

    fn key_remind(
        &mut self,
        key: Key,
        thread_id: String,
        title: String,
        custom: Option<String>,
    ) -> Option<Effect> {
        let now = haste_core::time::now_secs();
        let off = self.tz_offset;
        let done = |me: &mut Self, due: Option<i64>| {
            me.mode = Mode::Normal;
            Some(Effect::ToReminder {
                thread_id: thread_id.clone(),
                title: title.clone(),
                due,
            })
        };
        match (key, custom) {
            (Key::Esc, _) => self.mode = Mode::Normal,
            // quick picks
            (Key::Enter, None) => return done(self, Some(self.suggested_due(&title))),
            (Key::Char('m'), None) => return done(self, Some(inbox::tomorrow_morning(now, off))),
            (Key::Char('w'), None) => return done(self, Some(inbox::next_week(now, off))),
            (Key::Char('l'), None) => return done(self, Some(inbox::later(now))),
            (Key::Char('n'), None) => return done(self, None),
            (Key::Char('c'), None) => {
                let text = inbox::stamp(self.suggested_due(&title), off);
                self.mode = Mode::Remind {
                    thread_id,
                    title,
                    custom: Some(text),
                };
            }
            // typing a date
            (Key::Enter, Some(input)) => match inbox::parse_due(&input, now, off) {
                Ok(due) => return done(self, due),
                Err(e) => {
                    self.status = e;
                    self.mode = Mode::Remind {
                        thread_id,
                        title,
                        custom: Some(input),
                    };
                }
            },
            (Key::Backspace, Some(mut input)) => {
                input.pop();
                self.mode = Mode::Remind {
                    thread_id,
                    title,
                    custom: Some(input),
                };
            }
            (Key::Char(c), Some(mut input)) => {
                input.push(c);
                self.mode = Mode::Remind {
                    thread_id,
                    title,
                    custom: Some(input),
                };
            }
            (_, custom) => {
                self.mode = Mode::Remind {
                    thread_id,
                    title,
                    custom,
                };
            }
        }
        None
    }

    fn key_reply_choice(&mut self, key: Key, thread_id: String) -> Option<Effect> {
        match key {
            Key::Char('r') | Key::Enter => {
                self.mode = Mode::Normal;
                return Some(Effect::Reply { thread_id });
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
            Mode::Remind {
                title,
                custom: None,
                ..
            } => Some(format!(
                "Remind me: Enter {} · m tomorrow morning · w next week · l later (+3h) · n no date · c type a date · Esc",
                inbox::describe(self.suggested_due(title), self.tz_offset)
            )),
            Mode::Remind {
                custom: Some(input),
                ..
            } => Some(format!(
                "Reminder due (local time; Enter sets, empty = no date, Esc cancels): {input}"
            )),
            Mode::ReplyChoice { .. } => {
                Some("Several people on this thread: r reply to sender · a reply to all · Esc".into())
            }
            Mode::ConfirmRelease(source) => Some(format!(
                "Release {source}? It will sync again from its saved cursor. [y/N]"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(id: &str, principal: &str, state: ProposalState, stale: bool) -> Proposal {
        Proposal {
            id: id.into(),
            idempotency_key: id.into(),
            principal_id: principal.into(),
            payload: Payload::Draft {
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

    fn remind_effect(app: &mut App, keys: &[Key]) -> Vec<Effect> {
        keys.iter().filter_map(|k| app.handle_key(*k)).collect()
    }

    #[test]
    fn t_offers_quick_picks_and_the_suggestion_is_the_default() {
        let now = haste_core::time::now_secs();
        let mut app = inbox_app(); // cursor on t2, "Team lunch": no date in the subject
        press(&mut app, "t");
        assert!(matches!(app.mode, Mode::Remind { custom: None, .. }));
        let prompt = app.confirm_prompt().unwrap();
        for hint in [
            "Enter",
            "m tomorrow",
            "w next week",
            "l later",
            "n no date",
            "c type",
        ] {
            assert!(prompt.contains(hint), "{hint}: {prompt}");
        }
        let Some(Effect::ToReminder { due: Some(due), .. }) = app.handle_key(Key::Enter) else {
            panic!("Enter accepts the suggestion")
        };
        let tomorrow = inbox::tomorrow_morning(now, app.tz_offset);
        assert!((due - tomorrow).abs() <= 1, "default is tomorrow morning");
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn each_quick_pick_key_sets_its_time() {
        let now = haste_core::time::now_secs();
        let pick = |key: char| {
            let mut app = inbox_app();
            press(&mut app, "t");
            let effects = remind_effect(&mut app, &[Key::Char(key)]);
            let Some(Effect::ToReminder { due, .. }) = effects.into_iter().next() else {
                panic!("{key}")
            };
            due
        };
        let morning = pick('m').unwrap();
        assert!((morning - inbox::tomorrow_morning(now, 0)).abs() <= 1);
        let week = pick('w').unwrap();
        assert!((week - inbox::next_week(now, 0)).abs() <= 1);
        let later = pick('l').unwrap();
        assert!((later - (now + 3 * 3600)).abs() <= 1);
        assert_eq!(pick('n'), None, "no date");
    }

    #[test]
    fn the_suggestion_uses_a_date_from_the_subject() {
        let mut app = App::new();
        app.set_threads(vec![mail_thread(
            "t9",
            "Contract due 2099-03-04",
            &["a@x.com"],
            1,
        )]);
        press(&mut app, "t");
        let Some(Effect::ToReminder { due: Some(due), .. }) = app.handle_key(Key::Enter) else {
            panic!()
        };
        assert_eq!(due, haste_core::time::from_civil(2099, 3, 4, 9, 0));
    }

    #[test]
    fn c_types_a_custom_date_and_empty_means_undated() {
        let mut app = inbox_app();
        press(&mut app, "tc");
        let Mode::Remind {
            custom: Some(text), ..
        } = app.mode.clone()
        else {
            panic!("{:?}", app.mode)
        };
        for _ in 0..text.len() {
            app.handle_key(Key::Backspace);
        }
        let effects = remind_effect(&mut app, &[Key::Enter]);
        assert_eq!(
            effects,
            vec![Effect::ToReminder {
                thread_id: "t2".into(),
                title: "Team lunch".into(),
                due: None
            }]
        );
        // a bad date keeps the prompt open
        press(&mut app, "tc");
        for _ in 0..40 {
            app.handle_key(Key::Backspace);
        }
        press(&mut app, "soon");
        assert!(remind_effect(&mut app, &[Key::Enter]).is_empty());
        assert!(matches!(app.mode, Mode::Remind { .. }));
        app.handle_key(Key::Esc);
        assert_eq!(app.mode, Mode::Normal);
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
    fn x_mutes_and_a_muted_sender_disappears_from_today_and_channels() {
        let mut app = inbox_app();
        assert_eq!(
            press(&mut app, "x"),
            vec![Effect::Mute {
                thread_id: "t2".into()
            }]
        );
        // the daemon answers with the pattern; the next refresh delivers the list
        app.set_ignore(&["^b@x\\.com$".to_owned()], vec![]);
        let ids: Vec<&str> = app
            .today_threads()
            .iter()
            .map(|t| t.thread.id.as_str())
            .collect();
        assert_eq!(ids, ["t1"], "t2 has b@x.com among its senders");
        press(&mut app, "2");
        assert_eq!(app.channel_threads(0).len(), 1);
        // an invalid line is reported and the rest still applies
        app.set_ignore(
            &["^b@x\\.com$".to_owned()],
            vec!["line 3: not a valid regular expression".into()],
        );
        assert_eq!(app.ignore_errors.len(), 1);
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
    fn the_reader_sorts_scrolls_replies_archives_and_closes() {
        let mut app = inbox_app();
        let view = app.threads[1].clone();
        app.show_thread(
            view,
            vec![
                message("m2", "b@x.com", "second", 20),
                message("m1", "a@x.com", "first", 10),
            ],
        );
        assert_eq!(app.mode, Mode::Read);
        let reader = app.reader.as_ref().unwrap();
        assert_eq!(reader.messages[0].id, "m1", "oldest first");
        assert!(reader.to_latest.get(), "opens on the newest message");
        // scrolling
        app.reader.as_ref().unwrap().scroll.set(5);
        press(&mut app, "j");
        assert_eq!(app.reader.as_ref().unwrap().scroll.get(), 6);
        press(&mut app, "k");
        press(&mut app, "b");
        assert_eq!(
            app.reader.as_ref().unwrap().scroll.get(),
            0,
            "never below the top"
        );
        press(&mut app, "G");
        assert_eq!(
            app.reader.as_ref().unwrap().scroll.get(),
            usize::MAX,
            "clamped when drawn"
        );
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
        app.show_thread(view, vec![]);
        assert!(app.handle_key(Key::Esc).is_none());
        assert_eq!(
            (app.mode.clone(), app.reader.is_none()),
            (Mode::Normal, true)
        );
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
            vec![Effect::Reply {
                thread_id: "t2".into()
            }]
        );
        press(&mut app, "j"); // t1: one other person, straight to the editor
        assert_eq!(
            press(&mut app, "r"),
            vec![Effect::Reply {
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
        let ids: Vec<&str> = app.agenda().iter().map(|x| x.event.id.as_str()).collect();
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
    fn the_agents_panel_lists_machines_then_workspaces_then_agents_and_hides_stale_rows() {
        let mut app = approvals_app();
        app.set_machines(vec![
            machine(
                "local",
                MachineState::Connected,
                vec![
                    agent("local", "notes", "w2:p1", AgentStatus::Idle, 1),
                    agent("local", "haste", "w1:p1", AgentStatus::Blocked, 1),
                ],
            ),
            machine(
                "m1",
                MachineState::Unreachable,
                vec![agent("m1", "stale", "w1:p1", AgentStatus::Blocked, 1)],
            ),
            machine(
                "mbp",
                MachineState::Mismatch {
                    expected: 22,
                    observed: 23,
                },
                vec![],
            ),
        ]);
        let rows = app.agent_rows();
        let shape: Vec<String> = rows
            .iter()
            .map(|r| match r {
                AgentRow::Machine { name, .. } => format!("M:{name}"),
                AgentRow::Workspace(l) => format!("W:{l}"),
                AgentRow::Agent(a) => format!("A:{}", a.pane_id),
            })
            .collect();
        assert_eq!(
            shape,
            ["M:local", "W:haste", "A:w1:p1", "W:notes", "A:w2:p1", "M:m1", "M:mbp"]
        );
        assert!(
            !shape.iter().any(|r| r.contains("stale")),
            "an unreachable machine's rows are never shown"
        );
        assert!(matches!(
            &rows[6],
            AgentRow::Machine {
                state: MachineState::Mismatch {
                    expected: 22,
                    observed: 23
                },
                ..
            }
        ));
    }

    #[test]
    fn the_agents_panel_is_reached_with_tab_navigates_with_vi_keys_and_offers_no_actions() {
        let mut app = App::new();
        app.set_machines(vec![machine(
            "local",
            MachineState::Connected,
            vec![agent("local", "haste", "w1:p1", AgentStatus::Working, 1)],
        )]);
        press(&mut app, "6");
        assert_eq!(app.view, View::Agents);
        press(&mut app, "G");
        assert_eq!(app.agents_selected, 2);
        press(&mut app, "g");
        assert_eq!(app.agents_selected, 0);
        for keys in ["a", "A", "s", "r", "x", "R", "E"] {
            assert!(press(&mut app, keys).is_empty(), "{keys:?}");
        }
        assert_eq!(app.mode, Mode::Normal);
        app.handle_key(Key::Esc);
        assert_eq!(app.view, View::Today);
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
        for expected in [View::Calendar, View::Today] {
            app.handle_key(Key::Tab);
            assert_eq!(app.view, expected);
        }
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
        press(&mut app, "5");
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
