//! Drawing. All state lives in [`crate::app::App`]. Colours come from [`crate::theme`].

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{summary, AgentRow, App, Mode, Row, TodayFocus, View};
use crate::inbox::{self, CHANNELS};
use crate::theme;
use haste_core::herdr::{AgentStatus, MachineState};
use haste_core::proposal::ProposalState;
use haste_proto::ThreadView;

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    frame.render_widget(Block::new().style(theme::text()), frame.area());
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);
    match app.view {
        View::Today => draw_today(frame, app, body),
        View::Channels => draw_channels(frame, app, body),
        View::Calendar => draw_calendar(frame, app, body),
        View::Approvals => draw_approvals(frame, app, body),
        View::Connectors => draw_connectors(frame, app, body),
        View::Agents => draw_agents(frame, app, body),
    }
    if let Mode::Detail(id) = &app.mode {
        draw_detail(frame, app, id, body);
    }
    if app.mode == Mode::Read {
        draw_reader(frame, app, body);
    }
    draw_footer(frame, app, footer);
}

fn draw_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let tab = |name: &str, active: bool| {
        if active {
            Span::styled(
                format!(" {name} "),
                Style::new()
                    .fg(theme::BASE)
                    .bg(theme::MAUVE)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {name} "), Style::new().fg(theme::SUBTEXT0))
        }
    };
    let mut spans = vec![
        Span::styled(
            " haste ",
            Style::new().fg(theme::MAUVE).add_modifier(Modifier::BOLD),
        ),
        tab("1 Today", app.view == View::Today),
        tab("2 Channels", app.view == View::Channels),
        tab("3 Calendar", app.view == View::Calendar),
        Span::styled(" │ ", theme::muted()),
    ];
    let pending = app.proposals.len();
    let approvals = format!("4 Approvals {pending}");
    spans.push(if pending > 0 && app.view != View::Approvals {
        Span::styled(
            format!(" {approvals} "),
            Style::new().fg(theme::PEACH).add_modifier(Modifier::BOLD),
        )
    } else {
        tab(&approvals, app.view == View::Approvals)
    });
    spans.push(tab(
        &format!("5 Connectors {}", app.connectors.len()),
        app.view == View::Connectors,
    ));
    spans.push(tab(
        &format!("6 Agents {}", app.blocked().len()),
        app.view == View::Agents,
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(theme::text()), area);
}

/// `(key, what)` hints for the footer, depending on where the cursor is.
fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    if app.mode == Mode::Read {
        return vec![
            ("j/k", "scroll"),
            ("Space/b", "page"),
            ("g/G", "top/end"),
            ("r", "reply"),
            ("e", "archive"),
            ("Esc", "close"),
        ];
    }
    let thread_keys = vec![
        ("e", "archive"),
        ("r", "reply"),
        ("g", "aside"),
        ("p", "priority"),
        ("s", "snooze"),
        ("t", "remind"),
        ("x", "mute"),
        ("f", "filter"),
    ];
    match app.view {
        View::Today if app.today_focus == TodayFocus::Todo => {
            vec![
                ("Space", "select"),
                ("e", "done (30 s to undo)"),
                ("Tab", "to incoming"),
                ("j/k", "move"),
            ]
        }
        View::Today => {
            let mut h = thread_keys;
            h.push(("Tab", "to TODOs"));
            h
        }
        View::Channels if app.channel_inner => {
            let mut h = thread_keys;
            h.push(("Esc", "back"));
            h
        }
        View::Channels => vec![("Enter", "open"), ("j/k", "move")],
        View::Calendar => vec![("j/k", "move"), ("Tab", "next tab")],
        View::Approvals => vec![
            ("a", "approve"),
            ("s", "send"),
            ("r", "reject"),
            ("x", "mark"),
            ("/", "filter"),
            ("Esc", "back"),
        ],
        View::Connectors => vec![("R", "release"), ("E", "engage"), ("Esc", "back")],
        View::Agents => vec![("Esc", "back")],
    }
}

fn draw_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let prompt = app
        .confirm_prompt()
        .or_else(|| (app.mode == Mode::Filter).then(|| format!("/{}", app.filter)));
    let mut lines = vec![Line::from(match prompt {
        Some(p) => Span::styled(
            p,
            Style::new().fg(theme::WARNING).add_modifier(Modifier::BOLD),
        ),
        None => {
            let mut spans = Vec::new();
            for (k, what) in hints(app) {
                spans.push(Span::styled(
                    format!(" {k}"),
                    Style::new()
                        .fg(theme::HINT_KEY)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    format!(" {what} "),
                    Style::new().fg(theme::HINT_TEXT),
                ));
            }
            spans.push(Span::styled(" q quit ", theme::muted()));
            if !app.status.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", app.status),
                    Style::new().fg(theme::TEXT),
                ));
            }
            return_footer(frame, app, area, spans);
            return;
        }
    })];
    if let Some(notice) = app.notices.last() {
        lines.push(Line::from(Span::styled(
            format!("! {notice}"),
            Style::new().fg(theme::DANGER),
        )));
    }
    frame.render_widget(Paragraph::new(lines).style(theme::text()), area);
}

fn return_footer(frame: &mut Frame<'_>, app: &App, area: Rect, spans: Vec<Span<'static>>) {
    let mut lines = vec![Line::from(spans)];
    if let Some(notice) = app.notices.last() {
        lines.push(Line::from(Span::styled(
            format!("! {notice}"),
            Style::new().fg(theme::DANGER),
        )));
    } else if let Some(e) = &app.filter_error {
        lines.push(Line::from(Span::styled(
            format!("filters.json: {e}"),
            Style::new().fg(theme::WARNING),
        )));
    }
    frame.render_widget(Paragraph::new(lines).style(theme::text()), area);
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

/// One character per channel, plain Unicode so it draws in any terminal font.
#[must_use]
pub fn source_icon(source: &str) -> &'static str {
    match source {
        "gmail" => "✉",
        "icloud" => "☁",
        "whatsapp" => "✆",
        "messenger" => "◉",
        "imessage" => "✎",
        _ => "•",
    }
}

fn thread_line(t: &ThreadView, now: i64) -> ListItem<'static> {
    let source = t.thread.source.as_name();
    let star = if t.triage.priority > 0 { "★" } else { " " };
    let aside = if t.triage.set_aside { "◌ " } else { "" };
    let age = human_duration(now.saturating_sub(t.thread.last_activity_at));
    let is_mail = inbox::is_mail(&source);
    // Mail: who wrote the last message, then the subject. Chats: the chat name is the subject.
    let sender = t
        .last
        .as_ref()
        .and_then(|l| l.sender.as_deref())
        .map(inbox::short_sender)
        .unwrap_or_default();
    let mut spans = vec![
        Span::styled(format!("{star} "), Style::new().fg(theme::PRIORITY)),
        Span::styled(
            format!("{} ", source_icon(&source)),
            Style::new().fg(theme::channel_colour(&source)),
        ),
    ];
    if is_mail && !sender.is_empty() {
        spans.push(Span::styled(
            format!("{sender} "),
            Style::new().fg(theme::SUBTEXT0),
        ));
    }
    spans.push(Span::styled(
        t.thread.subject.clone(),
        Style::new().fg(theme::UNREAD),
    ));
    if let Some(last) = &t.last {
        let who = if is_mail {
            String::new()
        } else {
            last.sender
                .as_deref()
                .map(inbox::short_sender)
                .map(|s| format!("{s}: "))
                .unwrap_or_default()
        };
        spans.push(Span::styled(
            format!("  {aside}{who}{}", inbox::sanitize(&last.snippet)),
            Style::new().fg(theme::MUTED),
        ));
    } else if !aside.is_empty() {
        spans.push(Span::styled(
            format!(" {aside}"),
            Style::new().fg(theme::MUTED),
        ));
    }
    spans.push(Span::styled(
        format!("  {age}"),
        Style::new().fg(theme::MUTED),
    ));
    ListItem::new(Line::from(spans))
}

fn filter_title(app: &App, what: &str) -> String {
    format!(
        " {what} · filter: {} ({}/{}) ",
        app.active_filter().name,
        app.filter_index + 1,
        app.filters.len()
    )
}

fn pane_border(active: bool) -> Style {
    Style::new().fg(if active {
        theme::BORDER_ACTIVE
    } else {
        theme::BORDER
    })
}

/// A scrolling list: the window start is remembered in `offset` so the list moves only when the
/// cursor leaves the visible rows.
fn draw_scrolling(
    frame: &mut Frame<'_>,
    area: Rect,
    block: Block<'_>,
    items: Vec<ListItem<'_>>,
    selected: Option<usize>,
    offset: &std::cell::Cell<usize>,
) {
    let rows = usize::from(area.height.saturating_sub(2));
    let start = inbox::window_start(offset.get(), selected.unwrap_or(0), rows);
    offset.set(start);
    let mut state = ListState::default()
        .with_offset(start)
        .with_selected(selected);
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(theme::selected())
            .highlight_symbol("▌ "),
        area,
        &mut state,
    );
}

fn draw_today(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let now = haste_core::time::now_secs();
    let off = app.tz_offset;
    let [top, bottom] = Layout::vertical([
        Constraint::Max(u16::try_from(inbox::INBOX_ROWS + 2).unwrap_or(27)),
        Constraint::Min(6),
    ])
    .areas(area);
    let inbox_focus = app.today_focus == TodayFocus::Inbox;

    // incoming messages: ten rows, scrollable
    let threads = app.today_threads();
    let shown = if threads.len() > inbox::INBOX_ROWS {
        format!(
            "showing {} of {}, j/k scrolls",
            inbox::INBOX_ROWS,
            threads.len()
        )
    } else {
        format!("{}", threads.len())
    };
    let title = format!(
        " Incoming · {shown} · filter: {} ({}/{}) ",
        app.active_filter().name,
        app.filter_index + 1,
        app.filters.len()
    );
    let items: Vec<ListItem<'_>> = threads.iter().map(|t| thread_line(t, now)).collect();
    draw_scrolling(
        frame,
        top,
        theme::panel(&title).border_style(pane_border(inbox_focus)),
        items,
        (inbox_focus && !threads.is_empty()).then_some(app.today_selected),
        &app.inbox_offset,
    );

    // TODOs for today and without a date
    let todos = app.todos();
    let items: Vec<ListItem<'_>> = todos
        .iter()
        .map(|r| {
            let due = r
                .reminder
                .due_at
                .map_or_else(|| "no date".to_owned(), |d| inbox::stamp(d, off));
            let overdue = r.reminder.due_at.is_some_and(|d| d < now);
            if let Some(deadline) = app.pending_done.get(&r.reminder.id) {
                let left = deadline.saturating_duration_since(app.now).as_secs();
                return ListItem::new(Line::from(vec![
                    Span::styled("✓ ", Style::new().fg(theme::GREEN)),
                    Span::styled(
                        r.reminder.title.clone(),
                        Style::new()
                            .fg(theme::MUTED)
                            .add_modifier(Modifier::CROSSED_OUT),
                    ),
                    Span::styled(
                        format!("  done in {left}s · e undoes"),
                        Style::new().fg(theme::GREEN),
                    ),
                ]));
            }
            let marked = app.todo_marked.contains(&r.reminder.id);
            ListItem::new(Line::from(vec![
                Span::styled(
                    if marked { "▣ " } else { "☐ " },
                    Style::new().fg(if marked { theme::PEACH } else { theme::GREEN }),
                ),
                Span::styled(r.reminder.title.clone(), Style::new().fg(theme::TEXT)),
                Span::styled(
                    format!("  {due}"),
                    Style::new().fg(if overdue { theme::DANGER } else { theme::MUTED }),
                ),
            ]))
        })
        .collect();
    let items = if items.is_empty() {
        vec![ListItem::new(Span::styled(
            "nothing due today, nothing without a date",
            theme::muted(),
        ))]
    } else {
        items
    };
    let todo_title = format!(
        " TODO · today and no date · {} · Tab switches pane ",
        todos.len()
    );
    draw_scrolling(
        frame,
        bottom,
        theme::panel(&todo_title).border_style(pane_border(!inbox_focus)),
        items,
        (!inbox_focus && !todos.is_empty()).then_some(app.todo_selected),
        &app.todo_offset,
    );
}

fn draw_channels(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Length(28), Constraint::Min(20)]).areas(area);
    let now = haste_core::time::now_secs();
    let channel_items: Vec<ListItem<'_>> = CHANNELS
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let n = app.channel_threads(i).len();
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("● {name:<10}"),
                    Style::new().fg(theme::channel_colour(name)),
                ),
                Span::styled(
                    format!("{n:>4}"),
                    Style::new().fg(if n > 0 { theme::TEXT } else { theme::MUTED }),
                ),
            ]))
        })
        .collect();
    let mut cs = ListState::default().with_selected(Some(app.channels_selected));
    frame.render_stateful_widget(
        List::new(channel_items)
            .block(
                theme::panel(&filter_title(app, "Channels"))
                    .border_style(pane_border(!app.channel_inner)),
            )
            .highlight_style(theme::selected())
            .highlight_symbol("▌ "),
        left,
        &mut cs,
    );
    let threads = app.channel_threads(app.channels_selected);
    let items: Vec<ListItem<'_>> = threads.iter().map(|t| thread_line(t, now)).collect();
    let title = CHANNELS.get(app.channels_selected).copied().unwrap_or("");
    draw_scrolling(
        frame,
        right,
        theme::panel(&format!(" {title} ")).border_style(pane_border(app.channel_inner)),
        items,
        (app.channel_inner && !threads.is_empty()).then_some(app.channel_thread_selected),
        &app.channel_offset,
    );
}

fn draw_calendar(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let off = app.tz_offset;
    let events = app.agenda();
    let mut items: Vec<ListItem<'_>> = Vec::new();
    let mut position = None;
    let mut last_day = i64::MIN;
    for (i, e) in events.iter().enumerate() {
        // all-day events are dates (midnight UTC), not moments: never shift them by the offset
        let (day, label) = if e.event.all_day {
            (e.event.start_at, inbox::day_label(e.event.start_at, 0))
        } else {
            (
                inbox::day_start(e.event.start_at, off),
                inbox::day_label(e.event.start_at, off),
            )
        };
        if day != last_day {
            last_day = day;
            items.push(ListItem::new(Line::from(Span::styled(
                label,
                Style::new()
                    .fg(theme::LAVENDER)
                    .add_modifier(Modifier::BOLD),
            ))));
        }
        if i == app.calendar_selected {
            position = Some(items.len());
        }
        let when = if e.event.all_day {
            "all day".to_owned()
        } else {
            inbox::stamp(e.event.start_at, off)[11..].to_owned()
        };
        let tags = if e.tags.is_empty() {
            String::new()
        } else {
            format!("  #{}", e.tags.join(" #"))
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("  {when:<8}"), Style::new().fg(theme::SAPPHIRE)),
            Span::styled(e.event.title.clone(), Style::new().fg(theme::TEXT)),
            Span::styled(tags, Style::new().fg(theme::MAUVE)),
        ])));
    }
    if items.is_empty() {
        items.push(ListItem::new(Span::styled(
            "nothing on the calendar for the next 7 days (or Calendar is not authorized yet)",
            theme::muted(),
        )));
    }
    let mut state = ListState::default().with_selected(position);
    frame.render_stateful_widget(
        List::new(items)
            .block(
                theme::panel(" Calendar · next 7 days · local time · read-only ")
                    .border_style(Style::new().fg(theme::BORDER_ACTIVE)),
            )
            .highlight_style(theme::selected())
            .highlight_symbol("▌ "),
        area,
        &mut state,
    );
}

/// The opened thread: every message, oldest first, wrapped to the panel and scrollable.
fn draw_reader(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(reader) = &app.reader else {
        return;
    };
    let off = app.tz_offset;
    let thread = &reader.view.thread;
    let title = format!(
        " {} · {} · {} message(s) ",
        inbox::sanitize(&thread.subject).replace('\n', " "),
        thread.source.as_name(),
        reader.messages.len()
    );
    frame.render_widget(Clear, area);
    let block = theme::panel(&title).border_style(pane_border(true));
    let inner = block.inner(area);
    let width = usize::from(inner.width).saturating_sub(2).max(10);
    let height = usize::from(inner.height);

    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut last_start = 0;
    for m in &reader.messages {
        last_start = lines.len();
        let who = m.sender_identifier.as_deref().unwrap_or("unknown sender");
        lines.push(Line::from(vec![
            Span::styled(
                inbox::sanitize(who).replace('\n', " "),
                Style::new().fg(theme::MAUVE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", inbox::stamp(m.sent_at, off)),
                Style::new().fg(theme::MUTED),
            ),
        ]));
        let text = if m.body_text.trim().is_empty() {
            "(no text)".to_owned()
        } else {
            inbox::sanitize(&m.body_text)
        };
        for l in inbox::wrap(&text, width) {
            lines.push(Line::from(Span::styled(l, Style::new().fg(theme::TEXT))));
        }
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no messages stored for this thread yet",
            theme::muted(),
        )));
    }
    let max_scroll = lines.len().saturating_sub(height);
    if reader.to_latest.get() {
        reader.to_latest.set(false);
        reader.scroll.set(last_start);
    }
    let scroll = reader.scroll.get().min(max_scroll);
    reader.scroll.set(scroll);
    let more = if max_scroll > 0 {
        format!(" {}/{} ", scroll + 1, lines.len())
    } else {
        String::new()
    };
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .block(block.title_bottom(more)),
        area,
    );
}

fn draw_approvals(frame: &mut Frame<'_>, app: &App, area: Rect) {
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
                Style::new().fg(theme::WARNING).add_modifier(Modifier::BOLD),
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
            .block(theme::panel(help))
            .highlight_style(theme::selected())
            .highlight_symbol("▌ "),
        area,
        &mut state,
    );
}

fn draw_connectors(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let items: Vec<ListItem<'_>> = app
        .connectors
        .iter()
        .map(|c| {
            let (marker, style) = if c.engaged {
                (
                    "ENGAGED ",
                    Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
                )
            } else {
                ("running ", Style::new().fg(theme::OK))
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
            .block(theme::panel("R release (asks) · E engage · u refresh"))
            .highlight_style(theme::selected())
            .highlight_symbol("▌ "),
        area,
        &mut state,
    );
}

fn draw_agents(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let now = haste_core::time::now_secs();
    let items: Vec<ListItem<'_>> = app
        .agent_rows()
        .iter()
        .map(|row| match row {
            AgentRow::Machine { name, state } => {
                let (text, color) = match state {
                    MachineState::Connected => (format!("● {name} · connected"), theme::OK),
                    MachineState::Unreachable => (format!("○ {name} · unreachable"), theme::DANGER),
                    MachineState::Mismatch { expected, observed } => (
                        format!("✕ {name} · protocol mismatch (expected {expected}, observed {observed})"),
                        theme::DANGER,
                    ),
                };
                ListItem::new(Line::from(Span::styled(text, Style::new().fg(color).add_modifier(Modifier::BOLD))))
            }
            AgentRow::Workspace(label) => ListItem::new(format!("  ▾ {label}")),
            AgentRow::Agent(a) => {
                let since = now.saturating_sub(a.status_since);
                let color = match a.status {
                    AgentStatus::Blocked => theme::WARNING,
                    AgentStatus::Working => theme::SAPPHIRE,
                    _ => theme::SUBTEXT0,
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
            .block(theme::panel(
                "Agents by machine and workspace · read-only · u refresh",
            ))
            .highlight_style(theme::selected())
            .highlight_symbol("▌ "),
        area,
        &mut state,
    );
}

fn draw_detail(frame: &mut Frame<'_>, app: &App, id: &str, area: Rect) {
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
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(theme::panel("Proposal (Esc to close)")),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Key;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn every_screen_renders_with_rounded_borders_and_the_tab_bar() {
        let mut app = App::new();
        for key in ['1', '2', '3', '4', '5', '6'] {
            app.handle_key(Key::Char(key));
            let text = render(&app);
            assert!(text.contains('╭') && text.contains('╯'), "{:?}", app.view);
            assert!(text.contains("Today") && text.contains("Calendar"));
        }
    }

    #[test]
    fn colours_come_only_from_the_theme() {
        for (name, src) in [
            ("ui.rs", include_str!("ui.rs")),
            ("app.rs", include_str!("app.rs")),
        ] {
            let body = src.split("#[cfg(test)]").next().unwrap();
            assert!(!body.contains("Color::Rgb"), "{name}");
            assert!(!body.contains("Color::Yellow"), "{name}");
        }
    }

    #[test]
    fn today_has_a_25_row_incoming_pane_above_the_todo_pane() {
        let mut app = App::new();
        app.set_threads(
            (0..40)
                .map(|i| {
                    let mut t = haste_core::model::Thread {
                        id: format!("t{i}"),
                        source: haste_core::model::Source::Gmail,
                        external_id: format!("g{i}"),
                        subject: format!("subject-{i:02}"),
                        participants: vec![],
                        last_activity_at: 1_000 + i64::from(i),
                        version: 1,
                    };
                    t.version = 1;
                    haste_proto::ThreadView {
                        triage: haste_core::model::Triage::new(
                            &t.id,
                            haste_core::model::ObjectKind::Thread,
                        ),
                        thread: t,
                        tags: vec![],
                        last: None,
                    }
                })
                .collect(),
        );
        let text = render(&app);
        assert!(text.contains("showing 25 of 40"), "{text}");
        assert!(text.contains("TODO"));
        // newest first: subject-39 is visible, the oldest are scrolled out
        assert!(text.contains("subject-39") && text.contains("subject-15"));
        assert!(!text.contains("subject-14"), "only 25 rows are shown");
        // scrolling down to the 30th item brings it into view
        for _ in 0..29 {
            app.handle_key(Key::Char('j'));
        }
        let text = render(&app);
        assert!(text.contains("subject-10"), "scrolled to the cursor");
    }

    fn open_thread(app: &mut App, messages: Vec<haste_core::model::Message>) {
        let view = haste_proto::ThreadView {
            thread: haste_core::model::Thread {
                id: "t1".into(),
                source: haste_core::model::Source::Gmail,
                external_id: "g1".into(),
                subject: "Quarterly plan".into(),
                participants: vec![],
                last_activity_at: 1,
                version: 1,
            },
            triage: haste_core::model::Triage::new("t1", haste_core::model::ObjectKind::Thread),
            tags: vec![],
            last: None,
        };
        app.show_thread(view, messages);
    }

    fn msg(id: &str, text: &str, at: i64) -> haste_core::model::Message {
        haste_core::model::Message {
            id: id.into(),
            thread_id: "t1".into(),
            source: haste_core::model::Source::Gmail,
            external_id: id.into(),
            sender_person_id: None,
            sender_identifier: Some("ana@x.org".into()),
            body_text: text.into(),
            sent_at: at,
        }
    }

    #[test]
    fn the_reader_shows_messages_wrapped_and_without_terminal_escapes() {
        let mut app = App::new();
        open_thread(
            &mut app,
            vec![
                msg("m1", "hello there", 100),
                msg(
                    "m2",
                    "Kedves Marcell, k\u{1b}[2Jöszönöm \u{202E}a levelet",
                    200,
                ),
            ],
        );
        let text = render(&app);
        assert!(text.contains("Quarterly plan") && text.contains("2 message(s)"));
        assert!(text.contains("ana@x.org") && text.contains("Kedves Marcell"));
        assert!(!text.contains('\u{1b}') && !text.contains('\u{202E}'));
        assert!(text.contains("köszönöm") || text.contains("k[2Jöszönöm"));
        assert!(text.contains("Esc") && text.contains("reply"));
    }

    #[test]
    fn a_long_thread_opens_on_the_newest_message_and_scrolls_to_the_older_ones() {
        let mut app = App::new();
        let many: Vec<_> = (0..40)
            .map(|i| {
                msg(
                    &format!("m{i}"),
                    &format!("body-number-{i:02}"),
                    100 + i64::from(i),
                )
            })
            .collect();
        open_thread(&mut app, many);
        let text = render(&app);
        assert!(text.contains("body-number-39"), "newest is visible");
        assert!(!text.contains("body-number-00"), "oldest is scrolled out");
        app.handle_key(Key::Char('g'));
        let text = render(&app);
        assert!(text.contains("body-number-00"), "g goes to the top");
        assert!(!text.contains("body-number-39"));
    }
}
