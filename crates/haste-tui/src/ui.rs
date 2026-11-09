//! Drawing. All state lives in [`crate::app::App`].

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{summary, AgentRow, App, Mode, Row, View};
use haste_core::herdr::{AgentStatus, MachineState};
use haste_core::proposal::ProposalState;

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    let tab = |name: &str, active: bool| {
        let style = if active {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        Span::styled(format!(" {name} "), style)
    };
    frame.render_widget(
        Line::from(vec![
            tab("Inbox", app.view == View::Inbox),
            tab(
                &format!("Approvals ({})", app.proposals.len()),
                app.view == View::Approvals,
            ),
            tab(
                &format!("Connectors ({})", app.connectors.len()),
                app.view == View::Connectors,
            ),
            tab(
                &format!("Agents ({})", app.blocked().len()),
                app.view == View::Agents,
            ),
            Span::raw("  Tab switch · j/k g/G move · q quit"),
        ]),
        header,
    );

    match app.view {
        View::Inbox => draw_inbox(frame, app, body),
        View::Approvals => draw_approvals(frame, app, body),
        View::Connectors => draw_connectors(frame, app, body),
        View::Agents => draw_agents(frame, app, body),
    }
    if let Mode::Detail(id) = &app.mode {
        draw_detail(frame, app, id, body);
    }

    let prompt = app
        .confirm_prompt()
        .or_else(|| (app.mode == Mode::Filter).then(|| format!("/{}", app.filter)));
    let mut lines = vec![Line::from(match prompt {
        Some(p) => Span::styled(
            p,
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        None => Span::raw(app.status.clone()),
    })];
    if let Some(notice) = app.notices.last() {
        lines.push(Line::from(Span::styled(
            format!("! {notice}"),
            Style::new().fg(Color::Red),
        )));
    }
    frame.render_widget(Paragraph::new(lines), footer);
}

fn draw_inbox(frame: &mut Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem<'_>> = app
        .inbox_threads()
        .iter()
        .map(|t| {
            ListItem::new(format!(
                "[{:<9}] p{} {}",
                t.thread.source.as_name(),
                t.triage.priority,
                t.thread.subject
            ))
        })
        .collect();
    let title = match &app.source_filter {
        Some(s) => format!("Inbox · {} (f cycles source)", s.as_name()),
        None => "Inbox · all sources (f cycles source)".to_owned(),
    };
    let mut state = ListState::default().with_selected(Some(app.inbox_selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::new().borders(Borders::ALL).title(title))
            .highlight_symbol("> "),
        area,
        &mut state,
    );
}

fn draw_approvals(frame: &mut Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem<'_>> = app
        .rows()
        .iter()
        .map(|row| match row {
            Row::Group {
                principal,
                count,
                collapsed,
            } => ListItem::new(Line::from(Span::styled(
                format!(
                    "{} {principal} ({count})",
                    if *collapsed { "▸" } else { "▾" }
                ),
                Style::new().add_modifier(Modifier::BOLD),
            ))),
            Row::BlockedGroup { count, collapsed } => ListItem::new(Line::from(Span::styled(
                format!(
                    "{} Blocked agents ({count}) · read-only",
                    if *collapsed { "▸" } else { "▾" }
                ),
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ))),
            Row::Blocked(i) => {
                let blocked = app.blocked();
                match blocked.get(*i) {
                    Some(a) => ListItem::new(format!(
                        "    ! {} · {} · {}",
                        a.machine,
                        a.workspace_label,
                        a.agent.as_deref().unwrap_or("agent")
                    )),
                    None => ListItem::new(""),
                }
            }
            Row::Item(i) => {
                let p = &app.proposals[*i];
                let mark = if app.marked.contains(&p.id) {
                    "[x]"
                } else {
                    "[ ]"
                };
                let state = match p.state {
                    ProposalState::Approved => "approved",
                    ProposalState::Executing => "NEEDS CHECK",
                    _ => "pending ",
                };
                let stale = if p.stale { " STALE" } else { "" };
                let project = p
                    .project
                    .as_deref()
                    .map(|x| format!(" [{x}]"))
                    .unwrap_or_default();
                ListItem::new(format!("  {mark} {state}{stale}{project} {}", summary(p)))
            }
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.approvals_selected));
    let help =
        "a approve · A batch · x mark · s send/run · m/b settle NEEDS CHECK · r reject · c collapse · P group by agent/project · / filter";
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::new().borders(Borders::ALL).title(help))
            .highlight_symbol("> "),
        area,
        &mut state,
    );
}

fn draw_connectors(frame: &mut Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem<'_>> = app
        .connectors
        .iter()
        .map(|c| {
            let (marker, style) = if c.engaged {
                (
                    "ENGAGED ",
                    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
                )
            } else {
                ("running ", Style::new().fg(Color::Green))
            };
            let loaded = if c.registered { "" } else { " (not loaded)" };
            let extra = match (&app.reminders, c.source.as_str()) {
                (Some(r), "reminders") => {
                    let permission = if r.permission == "granted" {
                        String::new()
                    } else {
                        format!(" · PERMISSION NEEDED ({})", r.permission)
                    };
                    format!(" · {} open / {} total{permission}", r.open, r.total)
                }
                _ => String::new(),
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::raw(format!("{}{loaded}{extra}", c.source)),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.connectors_selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .title("R release (asks) · E engage · u refresh"),
            )
            .highlight_symbol("> "),
        area,
        &mut state,
    );
}

fn draw_agents(frame: &mut Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    let now = haste_core::time::now_secs();
    let items: Vec<ListItem<'_>> = app
        .agent_rows()
        .iter()
        .map(|row| match row {
            AgentRow::Machine { name, state } => {
                let (text, color) = match state {
                    MachineState::Connected => (format!("● {name} · connected"), Color::Green),
                    MachineState::Unreachable => (format!("○ {name} · unreachable"), Color::Red),
                    MachineState::Mismatch { expected, observed } => (
                        format!("✕ {name} · protocol mismatch (expected {expected}, observed {observed})"),
                        Color::Red,
                    ),
                };
                ListItem::new(Line::from(Span::styled(text, Style::new().fg(color).add_modifier(Modifier::BOLD))))
            }
            AgentRow::Workspace(label) => ListItem::new(format!("  ▾ {label}")),
            AgentRow::Agent(a) => {
                let since = now.saturating_sub(a.status_since);
                let color = match a.status {
                    AgentStatus::Blocked => Color::Yellow,
                    AgentStatus::Working => Color::Cyan,
                    _ => Color::Reset,
                };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("      {:<12}", a.agent.as_deref().unwrap_or("agent"))),
                    Span::styled(format!("{:<9}", a.status.as_str()), Style::new().fg(color)),
                    Span::raw(human_duration(since)),
                ]))
            }
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.agents_selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .title("Agents by machine and workspace · read-only · u refresh"),
            )
            .highlight_symbol("> "),
        area,
        &mut state,
    );
}

/// `45s`, `12m`, `3h`, `2d`.
fn human_duration(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

fn draw_detail(frame: &mut Frame<'_>, app: &App, id: &str, area: ratatui::layout::Rect) {
    let Some(p) = app.proposals.iter().find(|p| p.id == id) else {
        return;
    };
    let text = format!(
        "id: {}\nfrom: {}\nstate: {}{}\nexpires_at: {}\n\n{}",
        p.id,
        p.principal_id,
        p.state.as_str(),
        if p.stale {
            " (STALE: thread changed)"
        } else {
            ""
        },
        p.expires_at,
        summary(p),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(
            Block::new()
                .borders(Borders::ALL)
                .title("Proposal (Esc to close)"),
        ),
        area,
    );
}
