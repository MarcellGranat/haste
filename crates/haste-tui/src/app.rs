//! Pure TUI state and key handling. No terminal access, so everything here is unit-testable.

use std::collections::BTreeSet;

use haste_core::herdr::{AgentStatus, AgentStatusRecord, MachineState};
use haste_core::model::Source;
use haste_core::proposal::{Payload, Proposal, ProposalState};
use haste_proto::{ConnectorView, Event, MachineView, RemindersStatusView, ThreadView};

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
    Inbox,
    Approvals,
    Connectors,
    Agents,
}

/// How the Approvals view groups proposals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Agent,
    Project,
}

/// Group name for proposals that carry no project.
pub const NO_PROJECT: &str = "no project";

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
    Send {
        id: String,
        acknowledge_stale: bool,
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
    ConfirmSend {
        id: String,
        recipient: String,
        source: String,
        stale: bool,
    },
    ConfirmExecute {
        id: String,
        what: String,
        stale: bool,
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
    pub source_filter: Option<Source>,
    pub inbox_selected: usize,
    pub approvals_selected: usize,
    pub connectors: Vec<ConnectorView>,
    /// Reminders permission and counts (no titles); `None` until first read.
    pub reminders: Option<RemindersStatusView>,
    pub connectors_selected: usize,
    pub machines: Vec<MachineView>,
    pub agents_selected: usize,
    pub group_by: GroupBy,
    pub blocked_collapsed: bool,
    pub filter: String,
    pub collapsed: BTreeSet<String>,
    pub marked: BTreeSet<String>,
    pub notices: Vec<String>,
    pub status: String,
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
            view: View::Approvals,
            mode: Mode::Normal,
            threads: Vec::new(),
            proposals: Vec::new(),
            source_filter: None,
            inbox_selected: 0,
            approvals_selected: 0,
            connectors: Vec::new(),
            reminders: None,
            connectors_selected: 0,
            machines: Vec::new(),
            agents_selected: 0,
            group_by: GroupBy::Agent,
            blocked_collapsed: false,
            filter: String::new(),
            collapsed: BTreeSet::new(),
            marked: BTreeSet::new(),
            notices: Vec::new(),
            status: String::new(),
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

    fn visible_threads(&self) -> Vec<&ThreadView> {
        self.threads
            .iter()
            .filter(|t| {
                self.source_filter
                    .as_ref()
                    .is_none_or(|s| &t.thread.source == s)
            })
            .collect()
    }

    #[must_use]
    pub fn inbox_len(&self) -> usize {
        self.visible_threads().len()
    }

    #[must_use]
    pub fn inbox_threads(&self) -> Vec<&ThreadView> {
        self.visible_threads()
    }

    fn clamp(&mut self) {
        let rows = self.rows().len();
        self.approvals_selected = self.approvals_selected.min(rows.saturating_sub(1));
        self.inbox_selected = self.inbox_selected.min(self.inbox_len().saturating_sub(1));
        self.connectors_selected = self
            .connectors_selected
            .min(self.connectors.len().saturating_sub(1));
        self.agents_selected = self
            .agents_selected
            .min(self.agent_rows().len().saturating_sub(1));
    }

    fn selected_proposal(&self) -> Option<&Proposal> {
        match self.rows().get(self.approvals_selected)? {
            Row::Item(i) => self.proposals.get(*i),
            Row::Group { .. } | Row::BlockedGroup { .. } | Row::Blocked(_) => None,
        }
    }

    fn source_of(&self, p: &Proposal) -> String {
        p.source_thread_id
            .as_ref()
            .and_then(|id| self.threads.iter().find(|t| &t.thread.id == id))
            .map_or_else(
                || "unknown source".to_owned(),
                |t| t.thread.source.as_name(),
            )
    }

    fn move_selection(&mut self, delta: isize) {
        let len = match self.view {
            View::Approvals => self.rows().len(),
            View::Inbox => self.inbox_len(),
            View::Connectors => self.connectors.len(),
            View::Agents => self.agent_rows().len(),
        };
        let sel = match self.view {
            View::Approvals => &mut self.approvals_selected,
            View::Inbox => &mut self.inbox_selected,
            View::Connectors => &mut self.connectors_selected,
            View::Agents => &mut self.agents_selected,
        };
        *sel = if len == 0 {
            0
        } else {
            sel.saturating_add_signed(delta).min(len - 1)
        };
    }

    fn jump(&mut self, last: bool) {
        let len = match self.view {
            View::Approvals => self.rows().len(),
            View::Inbox => self.inbox_len(),
            View::Connectors => self.connectors.len(),
            View::Agents => self.agent_rows().len(),
        };
        let target = if last { len.saturating_sub(1) } else { 0 };
        match self.view {
            View::Approvals => self.approvals_selected = target,
            View::Inbox => self.inbox_selected = target,
            View::Connectors => self.connectors_selected = target,
            View::Agents => self.agents_selected = target,
        }
    }

    fn cycle_source(&mut self) {
        let mut sources: Vec<Source> = self
            .threads
            .iter()
            .map(|t| t.thread.source.clone())
            .collect();
        sources.sort_by_key(Source::as_name);
        sources.dedup();
        self.source_filter = match &self.source_filter {
            None => sources.first().cloned(),
            Some(current) => sources
                .iter()
                .position(|s| s == current)
                .and_then(|i| sources.get(i + 1))
                .cloned(),
        };
        self.inbox_selected = 0;
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
            Mode::ConfirmSend { id, stale, .. } => self.confirm(
                key,
                Effect::Send {
                    id,
                    acknowledge_stale: stale,
                },
            ),
            Mode::ConfirmExecute { id, stale, .. } => self.confirm(
                key,
                Effect::Execute {
                    id,
                    acknowledge_stale: stale,
                },
            ),
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
        match key {
            Key::Char('q') => return Some(Effect::Quit),
            Key::Tab => {
                self.view = match self.view {
                    View::Inbox => View::Approvals,
                    View::Approvals => View::Connectors,
                    View::Connectors => View::Agents,
                    View::Agents => View::Inbox,
                };
            }
            Key::Char('P') if self.view == View::Approvals => self.toggle_group_by(),
            Key::Char('R') if self.view == View::Connectors => self.start_release(),
            Key::Char('E') if self.view == View::Connectors => return self.engage_selected(),
            Key::Char('j') | Key::Down => self.move_selection(1),
            Key::Char('k') | Key::Up => self.move_selection(-1),
            Key::Char('g') => self.jump(false),
            Key::Char('G') => self.jump(true),
            Key::Char('r') if self.view == View::Approvals => return self.reject_selected(),
            Key::Char('u') => return Some(Effect::Refresh),
            Key::Char('f') if self.view == View::Inbox => self.cycle_source(),
            Key::Char('/') if self.view == View::Approvals => {
                self.mode = Mode::Filter;
                self.filter.clear();
            }
            Key::Char('x' | ' ') if self.view == View::Approvals => self.toggle_mark(),
            Key::Char('c') if self.view == View::Approvals => self.toggle_group(),
            Key::Enter if self.view == View::Approvals => self.enter(),
            Key::Char('a') if self.view == View::Approvals => return self.approve_selected(),
            Key::Char('A') if self.view == View::Approvals => self.start_batch(),
            Key::Char('s') if self.view == View::Approvals => self.start_send(),
            Key::Char('T') if self.view == View::Inbox => return self.thread_to_reminder(),
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

    fn start_send(&mut self) {
        let Some(p) = self.selected_proposal().cloned() else {
            return;
        };
        if p.payload.is_reminder() || p.payload.is_calendar() {
            self.start_execute(&p);
            return;
        }
        if p.state != ProposalState::Approved {
            self.status = "send is only available for approved proposals".into();
            return;
        }
        let recipient = match &p.payload {
            Payload::Draft { to, .. } => {
                to.clone().unwrap_or_else(|| "(thread participants)".into())
            }
            _ => {
                self.status = "only drafts can be sent".into();
                return;
            }
        };
        let source = self.source_of(&p);
        self.mode = Mode::ConfirmSend {
            id: p.id,
            recipient,
            source,
            stale: p.stale,
        };
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
                self.status = format!(
                    "outcome unknown: check the app (Reminders or Google Calendar), then `haste proposal resolve {} [--done]`",
                    p.id
                );
            }
            _ => self.status = "approve it first (a), then run it (s)".into(),
        }
    }

    /// `T` in the inbox: propose a reminder about the selected thread, titled by its subject.
    fn thread_to_reminder(&mut self) -> Option<Effect> {
        let thread = self.visible_threads().get(self.inbox_selected).copied()?;
        Some(Effect::ToReminder {
            thread_id: thread.thread.id.clone(),
            title: thread.thread.subject.clone(),
        })
    }

    pub fn set_reminders(&mut self, status: RemindersStatusView) {
        self.reminders = Some(status);
    }

    /// Prompt line describing a pending confirmation, if any.
    #[must_use]
    pub fn confirm_prompt(&self) -> Option<String> {
        match &self.mode {
            Mode::ConfirmBatch(ids) => Some(format!("Approve {} proposal(s)? [y/N]", ids.len())),
            Mode::ConfirmRelease(source) => Some(format!(
                "Release {source}? It will sync again from its saved cursor. [y/N]"
            )),
            Mode::ConfirmStaleApprove(id) => {
                Some(format!("Thread changed. Approve {id} anyway? [y/N]"))
            }
            Mode::ConfirmSend {
                recipient, source, ..
            } => Some(format!("SEND to {recipient} via {source}? [y/N]")),
            Mode::ConfirmExecute { what, .. } => Some(format!("Run: {what}? [y/N]")),
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
        let mut app = App::new();
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
        assert!(app.status.contains("haste proposal resolve r1"));
        assert_eq!(app.rows().len(), 3, "it stays listed as open");
    }

    #[test]
    fn t_in_the_inbox_proposes_a_reminder_about_the_selected_thread() {
        let mut app = App::new();
        app.view = View::Inbox;
        app.set_threads(vec![ThreadView {
            thread: haste_core::model::Thread {
                id: "t1".into(),
                source: Source::Gmail,
                external_id: "g1".into(),
                subject: "Quarterly plan".into(),
                participants: vec![],
                last_activity_at: 1,
                version: 1,
            },
            triage: haste_core::model::Triage::new("t1", haste_core::model::ObjectKind::Thread),
            tags: vec![],
        }]);
        assert_eq!(
            press(&mut app, "T"),
            vec![Effect::ToReminder {
                thread_id: "t1".into(),
                title: "Quarterly plan".into()
            }]
        );
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
    fn send_is_only_offered_for_approved_proposals_and_names_recipient_and_source() {
        let mut app = app_with(vec![
            proposal("p1", "agent-a", ProposalState::Pending, false),
            proposal("p2", "agent-a", ProposalState::Approved, false),
        ]);
        press(&mut app, "j"); // p1 pending
        press(&mut app, "s");
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.status.contains("approved"));
        press(&mut app, "js");
        let prompt = app.confirm_prompt().unwrap();
        assert!(prompt.contains("ana@x.org"), "{prompt}");
        assert!(prompt.contains("unknown source"), "{prompt}");
        let effect = app.handle_key(Key::Char('y')).unwrap();
        assert_eq!(
            effect,
            Effect::Send {
                id: "p2".into(),
                acknowledge_stale: false
            }
        );
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
        let mut app = App::new();
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
        // the app starts on Approvals: Connectors, then Agents
        for _ in 0..2 {
            app.handle_key(Key::Tab);
        }
        assert_eq!(app.view, View::Agents);
        press(&mut app, "G");
        assert_eq!(app.agents_selected, 2);
        press(&mut app, "g");
        assert_eq!(app.agents_selected, 0);
        for keys in ["a", "A", "s", "r", "x", "R", "E"] {
            assert!(press(&mut app, keys).is_empty(), "{keys:?}");
        }
        assert_eq!(app.mode, Mode::Normal);
        app.handle_key(Key::Tab);
        assert_eq!(app.view, View::Inbox);
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
        let mut app = App::new();
        app.set_connectors(connectors());
        app.handle_key(Key::Tab); // approvals -> connectors
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
