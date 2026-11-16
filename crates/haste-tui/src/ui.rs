//! Drawing. All state lives in [`crate::app::App`]. Colours come from [`crate::theme`].

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::Frame;

use crate::app::{summary, App, Mode, Row, TodayFocus, View};
use crate::inbox::{self, CHANNELS};
use crate::theme;
use haste_core::proposal::ProposalState;
use haste_proto::ThreadView;

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    frame.render_widget(Block::new().style(theme::text()), frame.area());
    // under the emoji keyboard the screen it was opened from stays as it was
    let base = match &app.mode {
        Mode::Emoji { back } => back.as_ref(),
        other => other,
    };
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
        View::Later => draw_later(frame, app, body),
        View::Muted => draw_muted(frame, app, body),
        View::Logs => draw_logs(frame, app, body),
        View::Approvals => draw_approvals(frame, app, body),
        View::Connectors => draw_connectors(frame, app, body),
    }
    if let Mode::Detail(id) = &base {
        draw_detail(frame, app, id, body);
    }
    if matches!(
        base,
        Mode::Read | Mode::Attachments | Mode::Preview | Mode::React { .. }
    ) {
        draw_reader(frame, app, body);
    }
    if matches!(base, Mode::React { .. }) {
        draw_emoji(frame, frame.area(), " React · press a key · Esc ");
    }
    if matches!(base, Mode::Attachments | Mode::Preview) {
        draw_attachments(frame, app, frame.area());
    }
    if *base == Mode::Preview {
        draw_preview(frame, app, frame.area());
    }
    if *base == Mode::RemindForm {
        draw_remind(frame, app, frame.area());
    }
    if *base == Mode::EventEdit {
        draw_event_edit(frame, app, frame.area());
    }
    if *base == Mode::TaskDuration {
        draw_task_duration(frame, app, frame.area());
    }
    if *base == Mode::Compose {
        let replying = app
            .compose
            .as_ref()
            .is_some_and(|c| matches!(c.kind, crate::compose::ComposeKind::Reply { .. }))
            && app.reader.is_some();
        if replying {
            // the thread on top, the editor docked at the bottom: reply with the thread in view
            let editor_h = (body.height / 3)
                .clamp(9, 16)
                .min(body.height.saturating_sub(4));
            let [top, bottom] =
                Layout::vertical([Constraint::Min(3), Constraint::Length(editor_h)]).areas(body);
            draw_reader(frame, app, top);
            draw_compose(frame, app, bottom, true);
        } else {
            draw_compose(frame, app, frame.area(), false);
        }
    }
    if *base == Mode::Label {
        draw_labels(frame, app, frame.area());
    }
    draw_footer(frame, app, footer);
    if matches!(app.mode, Mode::Emoji { .. }) {
        draw_emoji(frame, frame.area(), " Emoji · press a key · Esc goes back ");
    }
}

/// A rectangle `w` x `h` (clamped) in the middle of `area`.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_compose(frame: &mut Frame<'_>, app: &App, area: Rect, docked: bool) {
    let Some(c) = &app.compose else {
        return;
    };
    let rect = if docked {
        area
    } else {
        centered(area, 90, area.height.saturating_sub(4).max(12))
    };
    frame.render_widget(Clear, rect);
    let heading = format!(" {} ", c.heading);
    let block = theme::panel(&heading).border_style(pane_border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let attach_rows = u16::try_from(c.attachments.len().min(4)).unwrap_or(4) + 1;
    let mut constraints: Vec<Constraint> = c
        .fields
        .iter()
        .map(|f| {
            if f.area.lines().len() == 1
                && !f.label.starts_with("Note")
                && !f.label.starts_with("Add")
            {
                Constraint::Length(3)
            } else {
                Constraint::Min(5)
            }
        })
        .collect();
    constraints.push(Constraint::Length(attach_rows));
    constraints.push(Constraint::Length(1));
    let areas = Layout::vertical(constraints).split(inner);
    for (i, f) in c.fields.iter().enumerate() {
        let focused = i == c.focus;
        let box_area = areas[i];
        let label = format!(" {} ", f.label);
        let b = theme::panel(&label).border_style(pane_border(focused));
        let text_area = b.inner(box_area);
        frame.render_widget(b, box_area);
        let lines: Vec<Line<'_>> = f
            .area
            .lines()
            .iter()
            .map(|l| {
                Line::from(Span::styled(
                    inbox::sanitize(l),
                    Style::new().fg(theme::TEXT),
                ))
            })
            .collect();
        // keep the cursor row visible
        let rows = usize::from(text_area.height).max(1);
        let top = f.area.row.saturating_sub(rows - 1);
        let scroll = u16::try_from(top).unwrap_or(0);
        frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), text_area);
        if focused && c.picker.is_none() {
            let x = text_area.x
                + u16::try_from(f.area.col)
                    .unwrap_or(0)
                    .min(text_area.width.saturating_sub(1));
            let y = text_area.y + u16::try_from(f.area.row - top).unwrap_or(0);
            frame.set_cursor_position((x, y));
        }
    }
    let n = c.fields.len();
    let mut attach: Vec<Line<'_>> = vec![Line::from(Span::styled(
        format!(
            "attachments: {}",
            if c.attachments.is_empty() { "none" } else { "" }
        ),
        theme::muted(),
    ))];
    for p in c.attachments.iter().take(4) {
        attach.push(Line::from(Span::styled(
            format!("  📎 {}", p.display()),
            Style::new().fg(theme::SUBTEXT0),
        )));
    }
    frame.render_widget(Paragraph::new(attach), areas[n]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Ctrl-S",
                Style::new()
                    .fg(theme::HINT_KEY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" save  ", Style::new().fg(theme::HINT_TEXT)),
            Span::styled(
                "Tab",
                Style::new()
                    .fg(theme::HINT_KEY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" next field  ", Style::new().fg(theme::HINT_TEXT)),
            Span::styled(
                "Ctrl-G",
                Style::new()
                    .fg(theme::HINT_KEY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" open in $EDITOR  ", Style::new().fg(theme::HINT_TEXT)),
            Span::styled(
                "Ctrl-F",
                Style::new()
                    .fg(theme::HINT_KEY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" attach  ", Style::new().fg(theme::HINT_TEXT)),
            Span::styled(
                "Ctrl-X",
                Style::new()
                    .fg(theme::HINT_KEY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" unattach  ", Style::new().fg(theme::HINT_TEXT)),
            Span::styled(
                "Esc",
                Style::new()
                    .fg(theme::HINT_KEY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" cancel", Style::new().fg(theme::HINT_TEXT)),
        ])),
        areas[n + 1],
    );
    if let Some(picker) = &c.picker {
        let prect = centered(area, 80, area.height.saturating_sub(8).max(8));
        frame.render_widget(Clear, prect);
        let matches = picker.matches();
        let title = format!(
            " Attach a file · {} found · Enter takes it, Esc closes ",
            matches.len()
        );
        let block = theme::panel(&title).border_style(pane_border(true));
        let inner = block.inner(prect);
        frame.render_widget(block, prect);
        let [q, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::new().fg(theme::MAUVE)),
                Span::styled(picker.query.clone(), Style::new().fg(theme::TEXT)),
            ])),
            q,
        );
        let home = std::env::var("HOME").unwrap_or_default();
        let items: Vec<ListItem<'_>> = matches
            .iter()
            .map(|p| {
                let shown = p.to_string_lossy().replacen(&home, "~", 1);
                ListItem::new(Span::styled(
                    inbox::sanitize(&shown),
                    Style::new().fg(theme::TEXT),
                ))
            })
            .collect();
        let mut st =
            ListState::default().with_selected((!items.is_empty()).then_some(picker.selected));
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(theme::selected())
                .highlight_symbol("▌ "),
            list,
            &mut st,
        );
        frame.set_cursor_position((
            q.x + 2 + u16::try_from(picker.query.chars().count()).unwrap_or(0),
            q.y,
        ));
    }
}

/// The attachments of the message under the cursor; while choosing where to save, the folder
/// picker on top.
/// The emoji keyboard: every letter and digit key but `y` and `z`, each with its emoji. `title`
/// says what a key does here (react, or type into the text).
fn draw_emoji(frame: &mut Frame<'_>, area: Rect, title: &str) {
    use unicode_width::UnicodeWidthStr;
    const KEY_W: usize = 7;
    let rows = crate::emoji::ROWS;
    let widest = rows.iter().map(|r| r.len()).max().unwrap_or(10);
    let w = u16::try_from(widest * KEY_W + 12).unwrap_or(80);
    let h = u16::try_from(rows.len() * 3 + 4).unwrap_or(16);
    let rect = centered(area, w.min(area.width), h.min(area.height));
    frame.render_widget(Clear, rect);
    let block = theme::panel(title).border_style(pane_border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let edge = Style::new().fg(theme::SURFACE2);
    let key_style = Style::new().fg(theme::MAUVE).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'static>> = vec![Line::default()];
    for (r, row) in rows.iter().enumerate() {
        // a keyboard's rows are shifted a little more each: 0, 2, 4, 6 columns
        let pad = " ".repeat(2 + r * 2);
        let top: String = row
            .iter()
            .map(|_| format!("┌{}┐", "─".repeat(KEY_W - 2)))
            .collect();
        let bottom: String = row
            .iter()
            .map(|_| format!("└{}┘", "─".repeat(KEY_W - 2)))
            .collect();
        lines.push(Line::from(Span::styled(format!("{pad}{top}"), edge)));
        let mut mid = vec![Span::raw(pad.clone())];
        for (key, emoji) in row.iter() {
            // letter, then the emoji centred in what is left of the key
            let space = (KEY_W - 2).saturating_sub(2 + emoji.width());
            mid.push(Span::styled("│", edge));
            mid.push(Span::styled(format!(" {key}"), key_style));
            mid.push(Span::raw(format!("{emoji}{}", " ".repeat(space))));
            mid.push(Span::styled("│", edge));
        }
        lines.push(Line::from(mid));
        lines.push(Line::from(Span::styled(format!("{pad}{bottom}"), edge)));
    }
    lines.push(Line::from(Span::styled(
        "  a key picks its emoji · Esc goes back without one",
        theme::muted(),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_attachments(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(menu) = &app.attach else {
        return;
    };
    let height = u16::try_from(menu.files.len().min(12)).unwrap_or(12) + 4;
    let rect = centered(area, 84, height.min(area.height));
    frame.render_widget(Clear, rect);
    let block = theme::panel(" Attachments · p preview · Enter/d download · D all · Esc ")
        .border_style(pane_border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let items: Vec<ListItem<'_>> = menu
        .files
        .iter()
        .map(|f| {
            let mut text = inbox::sanitize(&f.name).replace('\n', " ");
            if !f.mime.is_empty() {
                text.push_str(&format!(" · {}", inbox::sanitize(&f.mime)));
            }
            if f.bytes > 0 {
                text.push_str(&format!(" · {}", crate::chat::human_size(f.bytes)));
            }
            let can_preview = crate::extract::kind_of(&f.name, &f.mime).is_some();
            ListItem::new(Line::from(vec![
                Span::styled("◫ ", Style::new().fg(theme::PEACH)),
                Span::styled(text, Style::new().fg(theme::TEXT)),
                Span::styled(
                    if can_preview { "  (preview)" } else { "" },
                    Style::new().fg(theme::MUTED),
                ),
            ]))
        })
        .collect();
    let mut st = ListState::default().with_selected(Some(menu.selected));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(theme::selected())
            .highlight_symbol("▌ "),
        inner,
        &mut st,
    );
    if let Some((picker, indices)) = &menu.picker {
        let prect = centered(area, 80, area.height.saturating_sub(8).max(8));
        frame.render_widget(Clear, prect);
        let matches = picker.matches();
        let what = if indices.len() == 1 {
            "1 file".to_owned()
        } else {
            format!("{} files", indices.len())
        };
        let title = format!(
            " Save {what} to a folder · {} found · Enter chooses, Esc back ",
            matches.len()
        );
        let block = theme::panel(&title).border_style(pane_border(true));
        let pinner = block.inner(prect);
        frame.render_widget(block, prect);
        let [q, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(pinner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::new().fg(theme::MAUVE)),
                Span::styled(picker.query.clone(), Style::new().fg(theme::TEXT)),
            ])),
            q,
        );
        let home = std::env::var("HOME").unwrap_or_default();
        let rows: Vec<ListItem<'_>> = matches
            .iter()
            .map(|p| {
                let shown = p.to_string_lossy().replacen(&home, "~", 1);
                ListItem::new(Span::styled(
                    inbox::sanitize(&shown),
                    Style::new().fg(theme::TEXT),
                ))
            })
            .collect();
        let mut pst =
            ListState::default().with_selected((!rows.is_empty()).then_some(picker.selected));
        frame.render_stateful_widget(
            List::new(rows)
                .highlight_style(theme::selected())
                .highlight_symbol("▌ "),
            list,
            &mut pst,
        );
        frame.set_cursor_position((
            q.x + 2 + u16::try_from(picker.query.chars().count()).unwrap_or(0),
            q.y,
        ));
    }
}

/// An attachment in a large popup: a picture as half-block art, or text in a reading column
/// with page rules, a scroll bar and the place in the file.
fn draw_preview(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(p) = &app.preview else {
        return;
    };
    let w = area.width.saturating_sub(8).max(30);
    let h = area.height.saturating_sub(4).max(8);
    let rect = centered(area, w, h);
    frame.render_widget(Clear, rect);
    if let Some(img) = &p.image {
        let title = format!(
            " {} · {}x{} px · Esc closes ",
            p.name, img.width, img.height
        );
        let block = theme::panel(&title).border_style(pane_border(true));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let cells = img.fit(usize::from(inner.width), usize::from(inner.height));
        let top = usize::from(inner.height).saturating_sub(cells.len()) / 2;
        let lines: Vec<Line<'_>> = std::iter::repeat_n(Line::default(), top)
            .chain(cells.iter().map(|row| {
                let pad = usize::from(inner.width).saturating_sub(row.len()) / 2;
                let mut spans = vec![Span::raw(" ".repeat(pad))];
                spans.extend(row.iter().map(|(up, down)| {
                    Span::styled(
                        "▀",
                        Style::new().fg(theme::pixel(*up)).bg(theme::pixel(*down)),
                    )
                }));
                Line::from(spans)
            }))
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let total = p.lines.len().max(1);
    let percent = ((p.scroll + 1) * 100 / total).min(100);
    let title = format!(
        " {} · line {}/{} · {percent}% · j/k scroll · Space/b page · g/G ends · Esc closes ",
        p.name,
        p.scroll + 1,
        total
    );
    let block = theme::panel(&title).border_style(pane_border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = usize::from(inner.height);
    // a reading column, centred, with room for the scroll bar
    let col = usize::from(inner.width.saturating_sub(2)).min(crate::app::PREVIEW_WIDTH + 2);
    let column = Rect {
        x: inner.x
            + u16::try_from((usize::from(inner.width).saturating_sub(col + 2)) / 2).unwrap_or(0),
        y: inner.y,
        width: u16::try_from(col).unwrap_or(inner.width),
        height: inner.height,
    };
    let lines: Vec<Line<'_>> = p
        .lines
        .iter()
        .skip(p.scroll)
        .take(rows)
        .map(|l| {
            if l.starts_with("── ") {
                Line::from(Span::styled(
                    l.clone(),
                    Style::new()
                        .fg(theme::LAVENDER)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(l.clone(), Style::new().fg(theme::TEXT)))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), column);
    let mut bar = ScrollbarState::new(total.saturating_sub(rows)).position(p.scroll);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight),
        inner,
        &mut bar,
    );
}

fn draw_labels(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(p) = &app.labeler else {
        return;
    };
    let rect = centered(area, 60, area.height.saturating_sub(6).clamp(8, 22));
    frame.render_widget(Clear, rect);
    let block = theme::panel(" Labels · Enter sets or clears · type a new one · Esc closes ")
        .border_style(pane_border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let [q, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::new().fg(theme::MAUVE)),
            Span::styled(p.input.clone(), Style::new().fg(theme::TEXT)),
        ])),
        q,
    );
    let rows = p.rows();
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|r| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    if r.on { "● " } else { "○ " },
                    Style::new().fg(if r.on { theme::GREEN } else { theme::MUTED }),
                ),
                Span::styled(inbox::sanitize(&r.label), Style::new().fg(theme::TEXT)),
            ]))
        })
        .collect();
    let mut st = ListState::default().with_selected((!items.is_empty()).then_some(p.selected));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(theme::selected())
            .highlight_symbol("▌ "),
        list,
        &mut st,
    );
    frame.set_cursor_position((
        q.x + 2 + u16::try_from(p.input.chars().count()).unwrap_or(0),
        q.y,
    ));
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
        tab(
            &format!("4 Later {}", app.later_threads().len()),
            app.view == View::Later,
        ),
        Span::styled(" │ ", theme::muted()),
    ];
    let pending = app.proposals.len();
    let approvals = format!("5 Approvals {pending}");
    spans.push(if pending > 0 && app.view != View::Approvals {
        Span::styled(
            format!(" {approvals} "),
            Style::new().fg(theme::PEACH).add_modifier(Modifier::BOLD),
        )
    } else {
        tab(&approvals, app.view == View::Approvals)
    });
    spans.push(tab(
        &format!("6 Connectors {}", app.connectors.len()),
        app.view == View::Connectors,
    ));
    spans.push(tab(
        &format!("7 Muted {}", app.muted_threads().len()),
        app.view == View::Muted,
    ));
    spans.push(tab("8 Logs", app.view == View::Logs));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(theme::text()), area);
}

/// `(key, what)` hints for the footer, depending on where the cursor is.
fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    if app.mode == Mode::Read {
        return vec![
            ("j/k", "message"),
            ("o", "older"),
            ("g/G", "first/last"),
            ("r", "reply"),
            ("e", "archive"),
            ("Esc", "close"),
        ];
    }
    let thread_keys = vec![
        ("Enter", "open"),
        ("e", "archive"),
        ("r", "reply"),
        ("l", "label"),
        ("g", "aside"),
        ("p", "priority"),
        ("P", "priority sender"),
        ("u", "read/unread"),
        ("s", "snooze"),
        ("t", "remind"),
        ("x", "mute"),
        ("b", "accept sender"),
        ("f", "filter"),
    ];
    match app.view {
        View::Today if app.today_focus == TodayFocus::Todo => {
            vec![
                ("Enter", "edit"),
                ("d", "duration"),
                ("c", "new"),
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
        View::Calendar => {
            let mut h = vec![("j/k", "move")];
            if app
                .agenda()
                .get(app.calendar_selected)
                .is_some_and(|r| matches!(r, crate::app::AgendaItem::Task { .. }))
            {
                h.extend([
                    ("Enter", "edit"),
                    ("d", "duration"),
                    ("e", "done (30 s to undo)"),
                ]);
            }
            h.push(("Tab", "next tab"));
            h
        }
        View::Later if app.later_focus == TodayFocus::Todo => vec![
            ("Enter", "edit"),
            ("d", "duration"),
            ("c", "new"),
            ("Space", "select"),
            ("e", "done (30 s to undo)"),
            ("Tab", "to threads"),
            ("j/k", "move"),
        ],
        View::Logs => vec![
            ("j/k", "scroll"),
            ("G", "follow the end"),
            ("g", "top"),
            ("w", "warnings only"),
            ("Esc", "back"),
        ],
        View::Muted => vec![
            ("b", "let the sender back"),
            ("d", "delete this mail"),
            ("Enter", "open (read only)"),
            ("Esc", "back"),
            ("j/k", "move"),
        ],
        View::Later => vec![
            ("Enter", "open"),
            ("g", "back to inbox"),
            ("s", "snooze again"),
            ("e", "archive"),
            ("u", "read/unread"),
            ("Tab", "to TODOs"),
            ("j/k", "move"),
        ],
        View::Approvals => vec![
            ("a", "approve"),
            ("s", "send"),
            ("r", "reject"),
            ("x", "mark"),
            ("/", "filter"),
            ("Esc", "back"),
        ],
        View::Connectors => vec![("R", "release"), ("E", "engage"), ("Esc", "back")],
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

fn thread_line(
    t: &ThreadView,
    now: i64,
    priority: bool,
    blocked: bool,
    tag: Option<String>,
) -> ListItem<'static> {
    let source = t.thread.source.as_name();
    let star = if priority { "★" } else { " " };
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
    // unread mail: a thin bar at the left edge of the row, and bold text
    let unread = t.unread && !blocked;
    let mut spans = vec![
        Span::styled(if unread { "▎" } else { " " }, Style::new().fg(theme::BLUE)),
        Span::styled(
            format!("{} ", if blocked { "⊘" } else { star }),
            Style::new().fg(if blocked {
                theme::DANGER
            } else if priority {
                theme::PEACH
            } else {
                theme::PRIORITY
            }),
        ),
        Span::styled(
            format!("{} ", source_icon(&source)),
            Style::new().fg(theme::channel_colour(&source)),
        ),
    ];
    // why it is on the Later tab (back at ..., set aside) comes first, where a long row
    // cannot cut it off
    if let Some(tag) = tag {
        spans.push(Span::styled(
            format!("{tag}  "),
            Style::new()
                .fg(theme::LAVENDER)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if is_mail && !sender.is_empty() {
        let style = if (priority || unread) && !blocked {
            Style::new().fg(theme::TEXT).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme::SUBTEXT0)
        };
        spans.push(Span::styled(format!("{sender} "), style));
    }
    let subject_style = if blocked {
        Style::new().fg(theme::DANGER)
    } else if priority {
        Style::new().fg(theme::TEXT).add_modifier(Modifier::BOLD)
    } else if t.unread {
        Style::new().fg(theme::UNREAD).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme::UNREAD)
    };
    spans.push(Span::styled(t.thread.subject.clone(), subject_style));
    if blocked {
        spans.push(Span::styled(" blocked", Style::new().fg(theme::DANGER)));
    }
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
        if last.has_attachment {
            spans.push(Span::styled(" ◫", Style::new().fg(theme::PEACH)));
        }
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
    // on the vivid orange of a priority row every word is dark
    if priority && !blocked {
        for s in &mut spans {
            s.style.fg = Some(if s.style.fg == Some(theme::MUTED) {
                theme::PRIORITY_INK_SOFT
            } else {
                theme::PRIORITY_INK
            });
        }
    }
    // one background per row: blocked, priority, answered, unread, in that order of weight
    let item = ListItem::new(Line::from(spans));
    if blocked {
        item.style(Style::new().bg(theme::BLOCKED_BG))
    } else if priority {
        item.style(Style::new().bg(theme::PRIORITY_BG))
    } else if t.replied {
        item.style(Style::new().bg(theme::REPLIED_BG))
    } else if t.unread {
        item.style(Style::new().bg(theme::UNREAD_BG))
    } else {
        item
    }
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
    rows_out: &std::cell::Cell<usize>,
) {
    let rows = usize::from(area.height.saturating_sub(2));
    rows_out.set(rows);
    let start = inbox::window_start(offset.get(), selected.unwrap_or(0), rows);
    offset.set(start);
    let mut state = ListState::default()
        .with_offset(start)
        .with_selected(selected);
    frame.render_stateful_widget(
        List::new(items).block(block).highlight_symbol("▌ "),
        area,
        &mut state,
    );
    // The selected row keeps the colour of its kind (priority, unread, blocked...) but lighter,
    // in bold, with grey text turned readable: a plain grey highlight left the greyed words of
    // the row unreadable.
    if let Some(sel) = selected {
        let y = usize::from(area.y) + 1 + sel.saturating_sub(start);
        if sel >= start && sel - start < rows {
            if let Ok(y) = u16::try_from(y) {
                let buf = frame.buffer_mut();
                for x in area.x.saturating_add(1)..area.right().saturating_sub(1) {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        let style = cell.style();
                        cell.set_style(
                            Style::new()
                                .bg(theme::lifted(
                                    style.bg.unwrap_or(ratatui::style::Color::Reset),
                                ))
                                .add_modifier(Modifier::BOLD),
                        );
                        if let Some(fg) = theme::readable_on_selection(style.fg) {
                            cell.set_fg(fg);
                        }
                    }
                }
            }
        }
    }
}

/// The reminder popup: title (with the dim suggestion), date, repeat, notes, labels, files.
fn draw_remind(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::remind::{DatePart, Field};
    let Some(form) = &app.remind else {
        return;
    };
    let rect = centered(area, 76.min(area.width.saturating_sub(4)), 15);
    frame.render_widget(Clear, rect);
    let block = theme::panel(" New reminder · Tab next · Enter saves · Esc cancels ")
        .border_style(pane_border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let label = |name: &str, field: Field| {
        let on = form.field == field;
        Span::styled(
            format!("{} {name:<8}", if on { "▌" } else { " " }),
            if on {
                Style::new().fg(theme::MAUVE).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme::SUBTEXT0)
            },
        )
    };
    let text = Style::new().fg(theme::TEXT);
    let dim = Style::new().fg(theme::OVERLAY0);
    let picked = Style::new()
        .fg(theme::BASE)
        .bg(theme::MAUVE)
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'_>> = Vec::new();
    // title: what was typed, or the suggestion dimmed (Right takes it as text)
    let mut title = vec![label("Title", Field::Title)];
    if form.title.is_empty() {
        title.push(Span::styled(
            inbox::sanitize(&form.suggestion).replace('\n', " "),
            dim,
        ));
        title.push(Span::styled("   → edit  Tab/Enter use", dim));
    } else {
        // the words that gave the date or the repeat stand out
        let found = Style::new()
            .fg(theme::SAPPHIRE)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        let mut at = 0;
        for &(s, e) in &form.marks {
            if s >= at && e <= form.title.len() {
                title.push(Span::styled(inbox::sanitize(&form.title[at..s]), text));
                title.push(Span::styled(inbox::sanitize(&form.title[s..e]), found));
                at = e;
            }
        }
        title.push(Span::styled(inbox::sanitize(&form.title[at..]), text));
    }
    lines.push(Line::from(title));
    // date: day │ hour │ minute
    let mut when = vec![label("When", Field::Date)];
    match form.date_parts() {
        Some((day, hour, minute)) => {
            let part = |s: String, p: DatePart| {
                let on = form.field == Field::Date && form.part == p;
                Span::styled(format!(" {s} "), if on { picked } else { text })
            };
            when.push(part(day, DatePart::Day));
            when.push(Span::styled("│", dim));
            when.push(part(hour, DatePart::Hour));
            when.push(Span::styled(":", dim));
            when.push(part(minute, DatePart::Minute));
            if form.field == Field::Date {
                when.push(Span::styled(
                    "  ←→ part  ↑↓ change  type a number  ⌫ no date",
                    dim,
                ));
            } else if form.due_from_title {
                when.push(Span::styled("  ← from the title", dim));
            }
        }
        None => when.push(Span::styled(
            "no date  (↑ or ↓ sets one)",
            if form.field == Field::Date { text } else { dim },
        )),
    }
    lines.push(Line::from(when));
    let mut early = vec![label("Early", Field::Early)];
    early.push(match (form.early_minutes(), form.due) {
        (_, None) => Span::styled("needs a date", dim),
        (Some(m), _) => Span::styled(format!("{} before", crate::remind::early_text(m)), text),
        (None, _) => Span::styled("none", dim),
    });
    if form.field == Field::Early {
        early.push(Span::styled("  ↑↓ change · ⌫ none", dim));
    }
    lines.push(Line::from(early));
    let mut length = vec![label("Length", Field::Duration)];
    length.extend(duration_spans(
        &form.duration,
        form.field == Field::Duration,
    ));
    if form.field == Field::Duration {
        length.push(Span::styled("  ↑↓ steps · ←→ unit · type a number", dim));
    }
    lines.push(Line::from(length));
    let mut priority = vec![label("Priority", Field::Priority)];
    for p in haste_proto::TaskPriority::ALL {
        let on = p == form.priority;
        priority.push(Span::styled(
            format!(" {} ", p.label()),
            if on {
                Style::new()
                    .fg(theme::BASE)
                    .bg(priority_color(p))
                    .add_modifier(Modifier::BOLD)
            } else {
                dim
            },
        ));
    }
    if form.field == Field::Priority {
        priority.push(Span::styled("  ←→ change · l m h u", dim));
    }
    lines.push(Line::from(priority));
    let mut repeat = vec![label("Repeat", Field::Repeat)];
    repeat.push(match form.repeat {
        Some(r) => Span::styled(
            format!(
                "every {} {}{}",
                r.every,
                r.freq.as_str(),
                if r.every == 1 { "" } else { "s" }
            ),
            text,
        ),
        None => Span::styled("does not repeat", dim),
    });
    if form.field == Field::Repeat {
        repeat.push(Span::styled("  ←→ unit  ↑↓ every", dim));
    } else if form.repeat_from_title {
        repeat.push(Span::styled("  ← from the title", dim));
    }
    lines.push(Line::from(repeat));
    let mut notes = vec![label("Notes", Field::Notes)];
    notes.push(if form.notes.is_empty() {
        Span::styled("(type to add)", dim)
    } else {
        Span::styled(inbox::sanitize(&form.notes).replace('\n', " ⏎ "), text)
    });
    if form.field == Field::Notes {
        notes.push(Span::styled("  Ctrl-g opens the editor", dim));
    }
    lines.push(Line::from(notes));
    let mut labels = vec![label("Labels", Field::Labels)];
    if form.labels.is_empty() {
        labels.push(Span::styled("(no project labels yet)", dim));
    }
    if !form.label_query.is_empty() {
        labels.push(Span::styled(
            format!("/{} ", inbox::sanitize(&form.label_query)),
            Style::new()
                .fg(theme::SAPPHIRE)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let shown = form.visible_labels();
    for (k, &i) in shown.iter().enumerate() {
        let (name, on) = &form.labels[i];
        let here = form.field == Field::Labels && form.label_cursor == k;
        labels.push(Span::styled(
            format!(" [{}] {name} ", if *on { "x" } else { " " }),
            if here {
                picked
            } else if *on {
                text
            } else {
                dim
            },
        ));
    }
    if !form.label_query.is_empty() && shown.is_empty() {
        labels.push(Span::styled("no match", dim));
    }
    if form.field == Field::Labels {
        labels.push(Span::styled(
            if form.label_query.is_empty() {
                " type to search · ←→ move · Space or Enter picks"
            } else {
                " ←→ move · Space or Enter picks · Esc clears"
            },
            dim,
        ));
    }
    lines.push(Line::from(labels));
    let mut files = vec![label("Files", Field::Files)];
    if form.attachments.is_empty() {
        files.push(Span::styled("none  (Ctrl-f adds a file)", dim));
    } else {
        let names: Vec<String> = form
            .attachments
            .iter()
            .map(|p| {
                p.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            })
            .collect();
        files.push(Span::styled(inbox::sanitize(&names.join(", ")), text));
        if form.field == Field::Files {
            files.push(Span::styled("  ⌫ removes the last", dim));
        }
    }
    lines.push(Line::from(files));
    frame.render_widget(Paragraph::new(lines), inner);
    // the cursor sits at the end of the typed title or notes
    let (row, typed) = match form.field {
        Field::Title => (0, Some(form.title.chars().count())),
        Field::Notes => (3, Some(form.notes.chars().count())),
        _ => (0, None),
    };
    if let Some(n) = typed {
        frame.set_cursor_position((inner.x + 11 + u16::try_from(n).unwrap_or(0), inner.y + row));
    }
    if let Some(picker) = &form.picker {
        let prect = centered(area, 80, area.height.saturating_sub(8).max(8));
        frame.render_widget(Clear, prect);
        let matches = picker.matches();
        let ptitle = format!(
            " Attach a file · {} found · Enter takes it, Esc closes ",
            matches.len()
        );
        let block = theme::panel(&ptitle).border_style(pane_border(true));
        let pinner = block.inner(prect);
        frame.render_widget(block, prect);
        let [q, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(pinner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::new().fg(theme::MAUVE)),
                Span::styled(picker.query.clone(), text),
            ])),
            q,
        );
        let home = std::env::var("HOME").unwrap_or_default();
        let items: Vec<ListItem<'_>> = matches
            .iter()
            .map(|p| {
                ListItem::new(Span::styled(
                    inbox::sanitize(&p.to_string_lossy().replacen(&home, "~", 1)),
                    text,
                ))
            })
            .collect();
        let mut st =
            ListState::default().with_selected((!items.is_empty()).then_some(picker.selected));
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(theme::selected())
                .highlight_symbol("▌ "),
            list,
            &mut st,
        );
    }
}

/// What the daemon has been doing, newest at the bottom; the cursor on the last line follows it.
fn draw_logs(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let off = app.tz_offset;
    let lines = app.shown_logs();
    let title = format!(
        " Logs · {} lines{} · j/k scroll · G follows · w warnings only ",
        lines.len(),
        if app.logs_problems {
            " (warnings and errors)"
        } else {
            ""
        }
    );
    if lines.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no log lines yet (is hasted running?)",
                theme::muted(),
            )))
            .block(theme::panel(&title).border_style(pane_border(true))),
            area,
        );
        return;
    }
    let items: Vec<ListItem<'_>> = lines
        .iter()
        .map(|l| {
            let local = l.at + off;
            let (_, _, _, h, m) = haste_core::time::to_civil(local);
            let s = local.rem_euclid(60);
            let level = match l.level.as_str() {
                "error" => Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
                "warn" => Style::new().fg(theme::WARNING),
                _ => theme::muted(),
            };
            let who = if l.proc == "hasted" {
                String::new()
            } else {
                format!("[{}] ", l.proc)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{h:02}:{m:02}:{s:02} "), theme::muted()),
                Span::styled(format!("{:<5} ", l.level), level),
                Span::styled(who, Style::new().fg(theme::LAVENDER)),
                Span::styled(
                    format!("{} ", inbox::sanitize(&l.component)),
                    Style::new().fg(theme::SAPPHIRE),
                ),
                Span::styled(
                    inbox::sanitize(&l.event),
                    Style::new().fg(theme::TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", inbox::sanitize(&l.fields)),
                    Style::new().fg(theme::SUBTEXT0),
                ),
            ]))
        })
        .collect();
    draw_scrolling(
        frame,
        area,
        theme::panel(&title).border_style(pane_border(true)),
        items,
        Some(app.logs_selected),
        &app.logs_offset,
        &app.list_rows,
    );
}

/// The last screen: mail from senders on the mute list, kept out of every other list.
fn draw_muted(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let now = haste_core::time::now_secs();
    let threads = app.muted_threads();
    let title = format!(
        " Muted · {} from muted senders · b lets the sender back · d deletes this mail ",
        threads.len()
    );
    if threads.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "nothing from a muted sender (x on a mail mutes its sender)",
                theme::muted(),
            )))
            .block(theme::panel(&title).border_style(pane_border(true))),
            area,
        );
        return;
    }
    let items: Vec<ListItem<'_>> = threads
        .iter()
        .map(|t| thread_line(t, now, false, true, None))
        .collect();
    draw_scrolling(
        frame,
        area,
        theme::panel(&title).border_style(pane_border(true)),
        items,
        Some(app.muted_selected),
        &app.muted_offset,
        &app.list_rows,
    );
}

/// The Later tab: set-aside and snoozed threads on top, TODO items with a later date below.
fn draw_later(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let now = haste_core::time::now_secs();
    let [top, bottom] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);
    let inbox_focus = app.later_focus == TodayFocus::Inbox;
    let threads = app.later_threads();
    let items: Vec<ListItem<'_>> = threads
        .iter()
        .map(|t| {
            let tag = match t.triage.snoozed_until {
                Some(until) if until > now => {
                    Some(format!("⏰ back {}", inbox::stamp(until, app.tz_offset)))
                }
                _ => Some("◌ set aside".to_owned()),
            };
            thread_line(t, now, app.is_priority(t), app.is_blocked(t), tag)
        })
        .collect();
    let title = format!(
        " Later · {} set aside or snoozed · g brings back ",
        threads.len()
    );
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "nothing is set aside or snoozed: g sets a thread aside, s snoozes it",
                theme::muted(),
            )))
            .block(theme::panel(&title).border_style(pane_border(inbox_focus))),
            top,
        );
    } else {
        draw_scrolling(
            frame,
            top,
            theme::panel(&title).border_style(pane_border(inbox_focus)),
            items,
            (inbox_focus).then_some(app.later_selected),
            &app.later_offset,
            &app.list_rows,
        );
    }
    let todos = app.later_todos();
    let items = todo_items(app, &todos, now);
    let items = if items.is_empty() {
        vec![ListItem::new(Span::styled(
            "no TODO with a date after tomorrow",
            theme::muted(),
        ))]
    } else {
        items
    };
    let todo_title = format!(
        " TODO · later than tomorrow · {} · Tab switches pane ",
        todos.len()
    );
    draw_scrolling(
        frame,
        bottom,
        theme::panel(&todo_title).border_style(pane_border(!inbox_focus)),
        items,
        (!inbox_focus && !todos.is_empty()).then_some(app.later_todo_selected),
        &app.later_todo_offset,
        &app.list_rows,
    );
}

/// The colour of a priority: grey, none, orange, red.
fn priority_color(p: haste_proto::TaskPriority) -> ratatui::style::Color {
    use haste_proto::TaskPriority::{High, Low, Middle, Urgent};
    match p {
        Low => theme::OVERLAY0,
        Middle => theme::LAVENDER,
        High => theme::PEACH,
        Urgent => theme::DANGER,
    }
}

/// TODO rows: the box, the title, the due time (red when overdue) and a `tomorrow` label.
fn todo_items<'a>(app: &App, todos: &[&haste_proto::ReminderView], now: i64) -> Vec<ListItem<'a>> {
    let off = app.tz_offset;
    todos
        .iter()
        .map(|r| {
            // a late TODO says how late it is; otherwise the date
            let due = r.reminder.due_at.map_or_else(
                || "no date".to_owned(),
                |d| {
                    if d < now {
                        inbox::overdue_text(now - d)
                    } else {
                        inbox::stamp(d, off)
                    }
                },
            );
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
            let mut spans = vec![
                Span::styled(
                    if marked { "▣ " } else { "☐ " },
                    Style::new().fg(if marked { theme::PEACH } else { theme::GREEN }),
                ),
                Span::styled(
                    inbox::sanitize(&r.reminder.title).replace('\n', " "),
                    Style::new().fg(theme::TEXT),
                ),
            ];
            // middle is the usual and says nothing
            match r.priority {
                haste_proto::TaskPriority::Middle => {}
                p => spans.push(Span::styled(
                    format!(
                        "  {}",
                        match p {
                            haste_proto::TaskPriority::Urgent => "‼ urgent",
                            haste_proto::TaskPriority::High => "! high",
                            _ => "↓ low",
                        }
                    ),
                    Style::new()
                        .fg(priority_color(p))
                        .add_modifier(Modifier::BOLD),
                )),
            }
            if !r.tags.is_empty() {
                spans.push(Span::styled(
                    format!("  #{}", r.tags.join(" #")),
                    Style::new().fg(theme::MAUVE),
                ));
            }
            if app.is_tomorrow(r) {
                spans.push(Span::styled(
                    "  tomorrow",
                    Style::new()
                        .fg(theme::LAVENDER)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            if let Some(rule) = r.repeat {
                spans.push(Span::styled(
                    format!(
                        "  ↻ every {}{}",
                        if rule.every == 1 {
                            String::new()
                        } else {
                            format!("{} ", rule.every)
                        },
                        rule.freq.as_str()
                    ),
                    Style::new().fg(theme::LAVENDER),
                ));
            }
            spans.push(Span::styled(
                format!("  {due}"),
                Style::new().fg(if overdue { theme::DANGER } else { theme::MUTED }),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect()
}

fn draw_today(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let now = haste_core::time::now_secs();
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
    let items: Vec<ListItem<'_>> = threads
        .iter()
        .map(|t| thread_line(t, now, app.is_priority(t), app.is_blocked(t), None))
        .collect();
    draw_scrolling(
        frame,
        top,
        theme::panel(&title).border_style(pane_border(inbox_focus)),
        items,
        (inbox_focus && !threads.is_empty()).then_some(app.today_selected),
        &app.inbox_offset,
        &app.list_rows,
    );

    // TODOs for today, tomorrow and without a date
    let todos = app.todos();
    let items = todo_items(app, &todos, now);
    let items = if items.is_empty() {
        vec![ListItem::new(Span::styled(
            "nothing due today or tomorrow, nothing without a date",
            theme::muted(),
        ))]
    } else {
        items
    };
    let todo_title = format!(
        " TODO · today, tomorrow and no date · {} · Tab switches pane ",
        todos.len()
    );
    draw_scrolling(
        frame,
        bottom,
        theme::panel(&todo_title).border_style(pane_border(!inbox_focus)),
        items,
        (!inbox_focus && !todos.is_empty()).then_some(app.todo_selected),
        &app.todo_offset,
        &app.list_rows,
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
    let items: Vec<ListItem<'_>> = threads
        .iter()
        .map(|t| thread_line(t, now, app.is_priority(t), app.is_blocked(t), None))
        .collect();
    let title = CHANNELS.get(app.channels_selected).copied().unwrap_or("");
    draw_scrolling(
        frame,
        right,
        theme::panel(&format!(" {title} ")).border_style(pane_border(app.channel_inner)),
        items,
        (app.channel_inner && !threads.is_empty()).then_some(app.channel_thread_selected),
        &app.channel_offset,
        &app.list_rows,
    );
}

/// One item of the week grid: where it sits and what it says.
struct Placed {
    day: usize,
    start: i64,
    end: i64,
    lane: usize,
    /// How many lanes the group of overlapping items it belongs to has.
    lanes: usize,
    label: String,
    task: bool,
    shifted: bool,
    index: usize,
}

/// Cut or pad a text to exactly `width` display columns.
fn fit(text: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push_str(&" ".repeat(width.saturating_sub(used.max(out.width()))));
    out
}

/// The calendar as a week: the next seven days without the weekend, one column each, hours
/// down the side, events and dated TODOs placed by their time, the selected one lit.
fn draw_calendar(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::app::AgendaItem;
    let off = app.tz_offset;
    let now = haste_core::time::now_secs();
    let today = inbox::day_start(now, off);
    let days: Vec<i64> = (0..inbox::AGENDA_DAYS)
        .map(|d| today + d * 86_400)
        .filter(|d| (((d + off).div_euclid(86_400) + 3).rem_euclid(7)) < 5)
        .collect();
    let rows = app.agenda();
    let block = theme::panel(
        " Calendar · next 7 days without the weekend · ←→ day · j/k item · Enter edits ",
    )
    .border_style(Style::new().fg(theme::BORDER_ACTIVE));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if days.is_empty() || inner.height < 8 || inner.width < 30 {
        return;
    }
    // place the items: timed ones in the grid, all-day ones in the strip above it
    let mut placed: Vec<Placed> = Vec::new();
    let mut all_day: Vec<Vec<(String, usize)>> = vec![Vec::new(); days.len()];
    for (index, row) in rows.iter().enumerate() {
        let at = row.at();
        let Some(day) = days.iter().position(|d| at >= *d && at < *d + 86_400) else {
            continue;
        };
        match row {
            AgendaItem::Event(e) if e.event.all_day => {
                all_day[day].push((inbox::sanitize(&e.event.title).replace('\n', " "), index));
            }
            AgendaItem::Event(e) => placed.push(Placed {
                day,
                start: e.event.start_at,
                end: e.event.end_at.max(e.event.start_at + 1_800),
                lane: 0,
                lanes: 1,
                label: inbox::sanitize(&e.event.title).replace('\n', " "),
                task: false,
                shifted: e.shifted,
                index,
            }),
            AgendaItem::Task { reminder, at } => placed.push(Placed {
                day,
                start: *at,
                end: *at + i64::from(reminder.minutes) * 60,
                lane: 0,
                lanes: 1,
                label: inbox::sanitize(&reminder.reminder.title).replace('\n', " "),
                task: true,
                shifted: false,
                index,
            }),
        }
    }
    // lanes: items that overlap sit side by side; each group of overlapping items is split
    // among its own lanes, the others keep the whole column
    for d in 0..days.len() {
        let mut idx: Vec<usize> = (0..placed.len()).filter(|k| placed[*k].day == d).collect();
        idx.sort_by_key(|k| placed[*k].start);
        let mut cluster: Vec<usize> = Vec::new();
        let mut cluster_end = i64::MIN;
        let mut ends: Vec<i64> = Vec::new();
        let close = |cluster: &mut Vec<usize>, ends: &mut Vec<i64>, placed: &mut Vec<Placed>| {
            let lanes = ends.len().max(1);
            for k in cluster.drain(..) {
                placed[k].lanes = lanes;
            }
            ends.clear();
        };
        for k in idx {
            if !cluster.is_empty() && placed[k].start >= cluster_end {
                close(&mut cluster, &mut ends, &mut placed);
                cluster_end = i64::MIN;
            }
            let lane = match ends.iter().position(|e| *e <= placed[k].start) {
                Some(l) => l,
                None => {
                    ends.push(0);
                    ends.len() - 1
                }
            };
            ends[lane] = placed[k].end;
            placed[k].lane = lane;
            cluster_end = cluster_end.max(placed[k].end);
            cluster.push(k);
        }
        close(&mut cluster, &mut ends, &mut placed);
    }
    // the hours shown: the working day, wider when something sits outside it
    let minute_of = |t: i64, day: usize| ((t - days[day]) / 60).clamp(0, 24 * 60);
    let first = placed
        .iter()
        .map(|p| minute_of(p.start, p.day) / 60)
        .min()
        .map_or(8, |h| h.min(8));
    let last = placed
        .iter()
        .map(|p| (minute_of(p.end, p.day) + 59) / 60)
        .max()
        .map_or(18, |h| h.max(18))
        .min(24);
    let strip = all_day.iter().map(Vec::len).max().unwrap_or(0).min(2);
    let head = 1 + strip;
    let detail = 2;
    let grid_h = usize::from(inner.height)
        .saturating_sub(head + detail)
        .max(1);
    let span = usize::try_from((last - first) * 60).unwrap_or(600);
    let per_row = [15_usize, 30, 60, 120, 240]
        .into_iter()
        .find(|m| span.div_ceil(*m) <= grid_h)
        .unwrap_or(240);
    let n_rows = span.div_ceil(per_row);
    let gutter = 6_usize;
    let width = usize::from(inner.width);
    let col_w = ((width.saturating_sub(gutter)) / days.len()).max(8);
    let lane_text = |text: &str, w: usize| fit(text, w);

    let mut lines: Vec<Line<'static>> = Vec::new();
    // day names
    let mut head_spans = vec![Span::raw(" ".repeat(gutter))];
    for (d, day) in days.iter().enumerate() {
        let label = inbox::day_label(*day, off);
        let is_today = *day == today;
        let _ = d;
        head_spans.push(Span::styled(
            fit(&format!(" {label}"), col_w),
            if is_today {
                Style::new()
                    .fg(theme::BASE)
                    .bg(theme::MAUVE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new()
                    .fg(theme::LAVENDER)
                    .add_modifier(Modifier::BOLD)
            },
        ));
    }
    lines.push(Line::from(head_spans));
    // all-day strip
    for s in 0..strip {
        let mut spans = vec![Span::styled(
            fit(if s == 0 { "all d." } else { "" }, gutter),
            theme::muted(),
        )];
        for items in &all_day {
            let (text, index) = match items.get(s) {
                Some((t, i)) if s + 1 == strip && items.len() > strip => {
                    (format!(" {t} +{}", items.len() - strip), Some(*i))
                }
                Some((t, i)) => (format!(" {t}"), Some(*i)),
                None => (String::new(), None),
            };
            let selected = index == Some(app.calendar_selected);
            spans.push(Span::styled(
                fit(&text, col_w),
                if selected {
                    Style::new()
                        .fg(theme::BASE)
                        .bg(theme::MAUVE)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(theme::SAPPHIRE)
                },
            ));
        }
        lines.push(Line::from(spans));
    }
    // the grid
    for r in 0..n_rows.min(grid_h) {
        let row_start = first * 60 + i64::try_from(r * per_row).unwrap_or(0);
        let row_end = row_start + i64::try_from(per_row).unwrap_or(60);
        let on_hour = row_start % 60 == 0;
        let mut spans = vec![Span::styled(
            fit(
                &if on_hour {
                    format!("{:02}:00", row_start / 60)
                } else {
                    String::new()
                },
                gutter,
            ),
            theme::muted(),
        )];
        for (d, _) in days.iter().enumerate() {
            let inner_w = col_w.saturating_sub(1);
            spans.push(Span::styled("│", Style::new().fg(theme::SURFACE1)));
            // the items on this row, left to right
            let mut hits: Vec<(usize, usize, &Placed)> = placed
                .iter()
                .filter(|p| {
                    p.day == d && minute_of(p.start, d) < row_end && minute_of(p.end, d) > row_start
                })
                .map(|p| {
                    let lane_w = (inner_w / p.lanes).max(1);
                    let x = p.lane * lane_w;
                    let w = if p.lane + 1 == p.lanes {
                        inner_w.saturating_sub(x)
                    } else {
                        lane_w
                    };
                    (x, w, p)
                })
                .collect();
            hits.sort_by_key(|(x, _, _)| *x);
            let mut at = 0;
            for (x, w, p) in hits {
                if x > at {
                    spans.push(Span::raw(" ".repeat(x - at)));
                }
                let selected = p.index == app.calendar_selected;
                let begins = minute_of(p.start, d) >= row_start || r == 0;
                let (_, _, _, h, m) = haste_core::time::to_civil(p.start + off);
                let text = if begins {
                    format!(
                        "{}{}{h:02}:{m:02} {}",
                        if p.shifted { "↦" } else { "" },
                        if p.task { "☐ " } else { "" },
                        p.label
                    )
                } else {
                    String::new()
                };
                let style = if selected {
                    Style::new()
                        .fg(theme::BASE)
                        .bg(theme::MAUVE)
                        .add_modifier(Modifier::BOLD)
                } else if p.task {
                    Style::new().fg(theme::PEACH).bg(theme::SURFACE0)
                } else {
                    Style::new().fg(theme::TEXT).bg(theme::SURFACE1)
                };
                spans.push(Span::styled(lane_text(&text, w), style));
                at = x + w;
            }
            if at < inner_w {
                spans.push(Span::raw(" ".repeat(inner_w - at)));
            }
        }
        lines.push(Line::from(spans));
    }
    // what is selected, in full
    while lines.len() < head + grid_h {
        lines.push(Line::default());
    }
    let selected = rows.get(app.calendar_selected);
    let describe = match selected {
        Some(AgendaItem::Event(e)) if e.event.all_day => format!(
            "{} · all day · {}",
            inbox::day_label(e.event.start_at, 0),
            inbox::sanitize(&e.event.title).replace('\n', " ")
        ),
        Some(AgendaItem::Event(e)) => {
            let end = inbox::stamp(e.event.end_at, off);
            format!(
                "{} {}–{} · {}{}",
                inbox::day_label(e.event.start_at, off),
                &inbox::stamp(e.event.start_at, off)[11..],
                &end[11..],
                inbox::sanitize(&e.event.title).replace('\n', " "),
                if e.shifted {
                    " · moved here only · R puts it back"
                } else {
                    ""
                }
            )
        }
        Some(AgendaItem::Task { reminder, at }) => format!(
            "{} {} · ☐ {}",
            inbox::day_label(*at, off),
            &inbox::stamp(*at, off)[11..],
            inbox::sanitize(&reminder.reminder.title).replace('\n', " ")
        ),
        None => "nothing on the calendar for these days (or Calendar is not authorized yet)".into(),
    };
    lines.push(Line::from(Span::styled(
        "─".repeat(width),
        Style::new().fg(theme::SURFACE1),
    )));
    lines.push(Line::from(Span::styled(
        fit(&describe, width),
        Style::new().fg(theme::TEXT),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A length of time as it is edited: its text, then the unit digits are typed in (the picked
/// one lit while the field has the cursor).
fn duration_spans(edit: &crate::durfield::DurationEdit, focused: bool) -> Vec<Span<'static>> {
    use crate::durfield::Unit;
    let text = Style::new().fg(theme::TEXT);
    let dim = Style::new().fg(theme::OVERLAY0);
    let picked = Style::new()
        .fg(theme::BASE)
        .bg(theme::MAUVE)
        .add_modifier(Modifier::BOLD);
    let chip = |name: &str, unit: Unit| {
        Span::styled(
            format!(" {name} "),
            if focused && edit.unit == unit {
                picked
            } else {
                dim
            },
        )
    };
    vec![
        Span::styled(
            format!(" {} ", edit.text()),
            if focused { picked } else { text },
        ),
        Span::raw("  "),
        chip("h", Unit::Hours),
        chip("min", Unit::Minutes),
    ]
}

/// The popup that sets how long a task takes: 5 minutes up to 4 hours.
fn draw_task_duration(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some((_, title, edit)) = &app.task_duration else {
        return;
    };
    let rect = centered(area, 66.min(area.width.saturating_sub(4)), 8);
    frame.render_widget(Clear, rect);
    let block =
        theme::panel(" How long does it take? · here only ").border_style(pane_border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let dim = Style::new().fg(theme::OVERLAY0);
    let mut row = vec![Span::raw("  ")];
    row.extend(duration_spans(edit, true));
    if !edit.typed().is_empty() {
        row.push(Span::styled(format!("  typed {}", edit.typed()), dim));
    }
    let lines = vec![
        Line::from(Span::styled(
            format!("  ☐ {}", inbox::sanitize(title)),
            Style::new().fg(theme::PEACH).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(row),
        Line::default(),
        Line::from(Span::styled(
            "  ↑↓ steps: 10 min, 30 min, 1-4 h (the most) · ←→ unit",
            dim,
        )),
        Line::from(Span::styled(
            "  type a number · Enter keeps it · Esc cancels",
            dim,
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The popup that moves or resizes the selected event here only.
fn draw_event_edit(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::eventedit::{Field, Part};
    let Some(form) = &app.event_form else {
        return;
    };
    let rect = centered(area, 64.min(area.width.saturating_sub(4)), 8);
    frame.render_widget(Clear, rect);
    let block = theme::panel(" Move or resize · here only, Google keeps its time ")
        .border_style(pane_border(true));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let text = Style::new().fg(theme::TEXT);
    let dim = Style::new().fg(theme::OVERLAY0);
    let picked = Style::new()
        .fg(theme::BASE)
        .bg(theme::MAUVE)
        .add_modifier(Modifier::BOLD);
    let label = |name: &str, on: bool| {
        Span::styled(
            format!("{} {name:<9}", if on { "▌" } else { " " }),
            if on {
                Style::new().fg(theme::MAUVE).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme::SUBTEXT0)
            },
        )
    };
    let (day, hour, minute) = form.start_parts();
    let part = |s: String, p: Part| {
        let on = form.field == Field::Start && form.part == p;
        Span::styled(format!(" {s} "), if on { picked } else { text })
    };
    let end = inbox::stamp(form.end(), app.tz_offset);
    let lines = vec![
        Line::from(Span::styled(
            format!("  {}", inbox::sanitize(&form.title)),
            Style::new()
                .fg(theme::LAVENDER)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            label("Starts", form.field == Field::Start),
            part(day, Part::Day),
            Span::styled("│", dim),
            part(hour, Part::Hour),
            Span::styled(":", dim),
            part(minute, Part::Minute),
        ]),
        {
            let mut row = vec![label("Lasts", form.field == Field::Duration)];
            row.extend(duration_spans(
                &form.duration,
                form.field == Field::Duration,
            ));
            row.push(Span::styled(format!("  ends {}", &end[11..]), dim));
            Line::from(row)
        },
        Line::default(),
        Line::from(Span::styled(
            "  Tab start/duration · ←→ part or unit · ↑↓ change · type a number · Enter saves",
            dim,
        )),
        Line::from(Span::styled(
            if form.shifted {
                "  R puts it back to Google's time"
            } else {
                ""
            },
            dim,
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The opened thread: every message, oldest first, wrapped to the panel and scrollable.
fn draw_reader(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(reader) = &app.reader else {
        return;
    };
    let thread = &reader.view.thread;
    let source = thread.source.as_name();
    let labels = if reader.view.tags.is_empty() {
        String::new()
    } else {
        format!(" · {}", reader.view.tags.join(", "))
    };
    let title = format!(
        " {} {} · {} · {}/{}{labels} ",
        source_icon(&source),
        inbox::sanitize(&thread.subject).replace('\n', " "),
        source,
        reader.selected + 1,
        reader.messages.len()
    );
    frame.render_widget(Clear, area);
    let block = theme::panel(&title).border_style(pane_border(true));
    let inner = block.inner(area);
    let height = usize::from(inner.height);
    let first = reader.first_visible();
    let (mut lines, laid) = crate::chat::layout(
        &reader.messages[first..],
        &crate::chat::Layout {
            width: usize::from(inner.width),
            selected: reader.selected.saturating_sub(first),
            hidden: first,
            own: reader.own.as_deref(),
            tz_offset: app.tz_offset,
            now: haste_core::time::now_secs(),
            expanded: &reader.expanded,
        },
    );
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no messages stored for this thread yet",
            theme::muted(),
        )));
    }
    let pages = reader.half_pages.replace(0);
    if pages != 0 {
        let step = isize::try_from((height / 2).max(1)).unwrap_or(1);
        let moved = isize::try_from(reader.scroll.get()).unwrap_or(0) + pages * step;
        reader.scroll.set(usize::try_from(moved).unwrap_or(0));
    }
    let scroll = match laid.get(reader.selected.saturating_sub(first)) {
        Some(l) => crate::chat::scroll_to(reader.scroll.get(), l, height, lines.len()),
        None => 0,
    };
    reader.scroll.set(scroll);
    *reader.spans.borrow_mut() = laid.iter().map(|l| (l.start, l.end)).collect();
    reader.view_height.set(height);
    *reader.links.borrow_mut() = laid.iter().flat_map(|l| l.links.clone()).collect();
    let hint =
        " j/k message · C-d/C-u scroll · a attachments · + react · x fold · o older · r reply · l label · e archive · Esc ";
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .block(block.title_bottom(hint)),
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

    #[test]
    fn a_selected_row_keeps_its_colour_and_its_grey_words_stay_readable() {
        use ratatui::style::Color;
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        let rows = |priority: bool| -> Vec<ListItem<'static>> {
            let fg = if priority {
                theme::PRIORITY_INK_SOFT
            } else {
                theme::MUTED
            };
            let bg = if priority {
                theme::PRIORITY_BG
            } else {
                theme::UNREAD_BG
            };
            vec![ListItem::new(Line::from(vec![
                Span::styled("word ", Style::new().fg(theme::TEXT)),
                Span::styled("grey", Style::new().fg(fg)),
            ]))
            .style(Style::new().bg(bg))]
        };
        for priority in [true, false] {
            terminal
                .draw(|f| {
                    draw_scrolling(
                        f,
                        f.area(),
                        theme::panel(" t "),
                        rows(priority),
                        Some(0),
                        &std::cell::Cell::new(0),
                        &std::cell::Cell::new(0),
                    );
                })
                .unwrap();
            let buf = terminal.backend().buffer().clone();
            // row 1, after the border and the two-cell cursor; "grey" starts at x = 3 + 5
            let cell = buf.cell((9, 1)).unwrap();
            let base = if priority {
                theme::PRIORITY_BG
            } else {
                theme::UNREAD_BG
            };
            assert_eq!(
                cell.bg,
                theme::lifted(base),
                "a lighter shade of its own colour"
            );
            if priority {
                assert_eq!(cell.fg, theme::PRIORITY_INK_SOFT, "dark on the orange");
            } else {
                assert_eq!(cell.fg, theme::TEXT, "grey text is turned readable");
            }
            assert_ne!(cell.bg, Color::Reset);
        }
    }

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
                        unread: false,
                        replied: false,
                        sender_known: true,
                        sender: None,
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
            unread: false,
            replied: false,
            sender_known: true,
            sender: None,
        };
        app.show_thread(view, messages, None);
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
            is_own: false,
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
        assert!(text.contains("Quarterly plan") && text.contains("2/2"));
        assert!(text.contains("ana@x.org") && text.contains("Kedves Marcell"));
        assert!(!text.contains('\u{1b}') && !text.contains('\u{202E}'));
        assert!(text.contains("köszönöm") || text.contains("k[2Jöszönöm"));
        assert!(text.contains("Esc") && text.contains("reply"));
    }

    #[test]
    fn the_later_tab_shows_why_a_thread_is_there_before_its_subject() {
        let mut t = haste_proto::ThreadView {
            thread: haste_core::model::Thread {
                id: "a".into(),
                source: haste_core::model::Source::Gmail,
                external_id: "a".into(),
                subject: "a-very-long-subject".into(),
                participants: vec![],
                last_activity_at: 1,
                version: 1,
            },
            triage: haste_core::model::Triage::new("a", haste_core::model::ObjectKind::Thread),
            tags: vec![],
            last: None,
            unread: false,
            replied: false,
            sender_known: true,
            sender: None,
        };
        t.triage.snoozed_until = Some(haste_core::time::now_secs() + 7_200);
        let mut app = App::new();
        app.set_threads(vec![t]);
        app.view = crate::app::View::Later;
        let text = render(&app);
        let (why, subject) = (
            text.find("back ").unwrap(),
            text.find("a-very-long").unwrap(),
        );
        assert!(why < subject, "{text}");
        app.threads[0].triage.snoozed_until = None;
        app.threads[0].triage.set_aside = true;
        let text = render(&app);
        assert!(text.find("set aside ").unwrap() < text.find("a-very-long").unwrap());
    }

    #[test]
    fn ctrl_d_and_ctrl_u_move_half_a_screen_in_the_inbox() {
        let mut app = App::new();
        app.set_threads(
            (0..60)
                .map(|i| {
                    let id = format!("t{i}");
                    haste_proto::ThreadView {
                        thread: haste_core::model::Thread {
                            id: id.clone(),
                            source: haste_core::model::Source::Gmail,
                            external_id: id.clone(),
                            subject: format!("subject-{i:02}"),
                            participants: vec![],
                            last_activity_at: 1000 - i64::from(i),
                            version: 1,
                        },
                        triage: haste_core::model::Triage::new(
                            &id,
                            haste_core::model::ObjectKind::Thread,
                        ),
                        tags: vec![],
                        last: None,
                        unread: false,
                        replied: false,
                        sender_known: true,
                        sender: None,
                    }
                })
                .collect(),
        );
        render(&app); // the draw tells the app how many rows the list shows
        let rows = app.list_rows.get();
        assert!(rows >= 4, "{rows}");
        app.handle_key(Key::Ctrl('d'));
        assert_eq!(app.today_selected, rows / 2);
        app.handle_key(Key::Ctrl('d'));
        assert_eq!(app.today_selected, 2 * (rows / 2));
        app.handle_key(Key::Ctrl('u'));
        assert_eq!(app.today_selected, rows / 2);
        for _ in 0..10 {
            app.handle_key(Key::Ctrl('u'));
        }
        assert_eq!(app.today_selected, 0, "stops at the top");
    }

    #[test]
    fn the_reminder_popup_shows_the_dim_suggestion_and_every_field() {
        let mut app = App::new();
        app.known_labels = vec!["haste".into()];
        app.set_threads(vec![haste_proto::ThreadView {
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
            tags: vec!["haste".into()],
            last: None,
            unread: false,
            replied: false,
            sender_known: true,
            sender: None,
        }]);
        app.handle_key(Key::Char('t'));
        let text = render(&app);
        for needle in [
            "New reminder",
            "Quarterly plan",
            "When",
            "Repeat",
            "Notes",
            "[x] haste",
            "Files",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        // a date typed in the title shows in the date row
        for c in "x every week".chars() {
            app.handle_key(Key::Char(c));
        }
        assert!(render(&app).contains("every 1 week"));
    }

    #[test]
    fn today_labels_tomorrows_todos_and_later_has_the_far_ones() {
        let todo = |id: &str, due: i64| haste_proto::ReminderView {
            reminder: haste_core::model::Reminder {
                id: id.into(),
                external_id: id.into(),
                title: format!("task-{id}"),
                notes_snippet: String::new(),
                due_at: Some(due),
                completed: false,
                list_name: "L".into(),
                remote_modified_at: 0,
                missing_since: None,
                version: 1,
            },
            tags: vec![],
            repeat: None,
            minutes: 30,
            priority: haste_proto::TaskPriority::Middle,
            early: None,
        };
        let now = haste_core::time::now_secs();
        let mut app = App::new();
        let mut soon = todo("soon", now + 60);
        soon.tags = vec!["work".into(), "haste".into()];
        app.set_reminder_list(vec![
            soon,
            todo("next", now + 86_400),
            todo("far", now + 5 * 86_400),
        ]);
        let text = render(&app);
        assert!(
            text.contains("#work #haste"),
            "labels show on the row: {text}"
        );
        assert!(text.contains("task-soon") && text.contains("task-next"));
        // a late one says how late
        let mut late = todo("late", now - 3 * 86_400);
        late.reminder.title = "task-late".into();
        app.set_reminder_list(vec![late]);
        let text = render(&app);
        assert!(text.contains("3 days overdue"), "{text}");
        app.set_reminder_list(vec![
            {
                let mut s = todo("soon", now + 60);
                s.tags = vec!["work".into(), "haste".into()];
                s
            },
            todo("next", now + 86_400),
            todo("far", now + 5 * 86_400),
        ]);
        let text = render(&app);
        assert!(text.contains("tomorrow"), "{text}");
        assert!(!text.contains("task-far"), "later ones are not on Today");
        app.view = crate::app::View::Later;
        let text = render(&app);
        assert!(
            text.contains("task-far") && !text.contains("task-soon"),
            "{text}"
        );
    }

    #[test]
    fn strangers_are_last_in_the_inbox_and_muted_mail_is_on_the_last_screen() {
        let thread = |id: &str, known: bool, at: i64| haste_proto::ThreadView {
            thread: haste_core::model::Thread {
                id: id.into(),
                source: haste_core::model::Source::Gmail,
                external_id: id.into(),
                subject: format!("subject-{id}"),
                participants: vec![],
                last_activity_at: at,
                version: 1,
            },
            triage: haste_core::model::Triage::new(id, haste_core::model::ObjectKind::Thread),
            tags: vec![],
            last: None,
            unread: false,
            replied: false,
            sender_known: known,
            sender: Some(format!("{id}@x.org")),
        };
        let mut app = App::new();
        // the stranger is the newest, yet it is listed after the accepted one
        app.set_threads(vec![
            thread("friend", true, 1),
            thread("stranger", false, 9),
            thread("noisy", true, 5),
        ]);
        app.set_ignore(&["^noisy@x\\.org$".to_owned()], vec![]);
        let today = render(&app);
        let (friend, stranger) = (
            today.find("subject-friend").unwrap(),
            today.find("subject-stranger").unwrap(),
        );
        assert!(friend < stranger, "blocked mail is at the bottom: {today}");
        assert!(
            !today.contains("subject-noisy"),
            "muted mail is not in the inbox"
        );
        assert!(today.contains("7 Muted 1"), "{today}");
        app.view = crate::app::View::Muted;
        let muted = render(&app);
        assert!(muted.contains("subject-noisy") && !muted.contains("subject-friend"));
    }

    #[test]
    fn the_emoji_keyboard_shows_every_key_but_y_and_z_with_its_emoji() {
        let mut app = App::new();
        app.set_threads(vec![]);
        app.mode = Mode::Emoji {
            back: Box::new(Mode::Filter),
        };
        let text = render(&app);
        for (key, emoji) in crate::emoji::ROWS.iter().flat_map(|r| r.iter()) {
            assert!(text.contains(*emoji), "{key}: {emoji}");
        }
        assert!(text.contains("Esc"), "{text}");
        assert!(!text.contains(" y") && !text.contains(" z"));
    }

    #[test]
    fn the_logs_screen_lists_the_daemons_lines_with_their_level() {
        let mut app = App::new();
        app.view = crate::app::View::Logs;
        app.set_logs(vec![
            crate::logs::LogLine {
                at: 1_794_762_040,
                level: "warn".into(),
                proc: "hasted".into(),
                component: "connector".into(),
                event: "call".into(),
                fields: "source=imessage outcome=ambiguous".into(),
            },
            crate::logs::LogLine {
                at: 1_794_762_041,
                level: "info".into(),
                proc: "haste-tui".into(),
                component: "tui".into(),
                event: "send_requested".into(),
                fields: String::new(),
            },
        ]);
        let text = render(&app);
        assert!(text.contains("Logs · 2 lines"), "{text}");
        assert!(text.contains("connector") && text.contains("outcome=ambiguous"));
        assert!(
            text.contains("[haste-tui]"),
            "other programs are marked: {text}"
        );
    }

    #[test]
    fn the_calendar_is_a_week_grid_of_weekdays_with_events_where_their_time_is() {
        let mut app = App::new();
        let now = haste_core::time::now_secs();
        let weekday = |t: i64| (t.div_euclid(86_400) + 3).rem_euclid(7);
        let mut day = now - now.rem_euclid(86_400);
        while weekday(day) >= 5 {
            day += 86_400;
        }
        let at = |h: i64| day + h * 3_600;
        let ev = |id: &str, start: i64, mins: i64| haste_proto::EventView {
            event: haste_core::model::Event {
                id: id.into(),
                external_id: id.into(),
                calendar: "c".into(),
                title: format!("e-{id}"),
                start_at: start,
                end_at: start + mins * 60,
                all_day: false,
                status: "confirmed".into(),
                attendees: vec![],
                location: String::new(),
                description_snippet: String::new(),
                updated_at: 0,
                missing_since: None,
                version: 1,
            },
            tags: vec![],
            shifted: id == "moved",
        };
        // two that overlap share the column; one is far outside the working day
        app.set_events(vec![
            ev("one", at(10), 60),
            ev("two", at(10) + 1_800, 60),
            ev("late", at(21), 60),
            ev("moved", at(14), 30),
        ]);
        app.view = crate::app::View::Calendar;
        let text = render(&app);
        for needle in ["e-late", "e-moved", "10:00", "10:30", "21:00"] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        assert!(text.contains("↦"), "a moved event is marked: {text}");
        assert!(
            !text.contains("Sat ") && !text.contains("Sun "),
            "no weekend: {text}"
        );
        assert!(text.contains("Enter edits"), "{text}");
    }

    #[test]
    fn the_duration_popup_shows_the_length_and_how_to_change_it() {
        let mut app = App::new();
        app.task_duration = Some((
            "r1".into(),
            "Pay rent".into(),
            crate::durfield::DurationEdit::new(120, 5, 240, true),
        ));
        app.mode = Mode::TaskDuration;
        let text = render(&app);
        for needle in [
            "How long",
            "Pay rent",
            "2 h",
            "10 min",
            "1-4 h (the most)",
            "type a number",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
    }

    #[test]
    fn the_move_popup_shows_the_start_and_the_duration() {
        let mut app = App::new();
        app.event_form = Some(Box::new(crate::eventedit::EventForm::new(
            "e1",
            "Standup",
            1_794_744_000,
            1_794_744_000 + 5_400,
            true,
            3_600,
        )));
        app.mode = Mode::EventEdit;
        let text = render(&app);
        for needle in [
            "Move or resize",
            "Standup",
            "Starts",
            "Lasts",
            "1 h 30 min",
            "Google keeps",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        assert!(text.contains("R puts it back"), "{text}");
    }

    #[test]
    fn unread_mail_has_a_bar_at_the_row_edge() {
        let thread = |id: &str, unread: bool| haste_proto::ThreadView {
            thread: haste_core::model::Thread {
                id: id.into(),
                source: haste_core::model::Source::Gmail,
                external_id: id.into(),
                subject: format!("subject-{id}"),
                participants: vec![],
                last_activity_at: 1,
                version: 1,
            },
            triage: haste_core::model::Triage::new(id, haste_core::model::ObjectKind::Thread),
            tags: vec![],
            last: None,
            unread,
            replied: false,
            sender_known: true,
            sender: None,
        };
        let mut app = App::new();
        app.set_threads(vec![thread("a", true)]);
        assert!(render(&app).contains('▎'));
        app.set_threads(vec![thread("a", false)]);
        assert!(!render(&app).contains('▎'));
    }

    #[test]
    fn ctrl_d_and_ctrl_u_scroll_a_tall_message() {
        let mut app = App::new();
        let body: Vec<String> = (0..120).map(|i| format!("line-number-{i:03}")).collect();
        open_thread(&mut app, vec![msg("m1", &body.join("\n"), 100)]);
        let first = render(&app);
        assert!(
            first.contains("line-number-000"),
            "opens at the top of the message"
        );
        assert!(!first.contains("line-number-119"));
        app.handle_key(Key::Ctrl('d'));
        let after = render(&app);
        assert!(!after.contains("line-number-000"), "scrolled down: {after}");
        for _ in 0..20 {
            app.handle_key(Key::Ctrl('d'));
            render(&app);
        }
        let end = render(&app);
        assert!(end.contains("line-number-119"), "reaches the end: {end}");
        for _ in 0..30 {
            app.handle_key(Key::Ctrl('u'));
            render(&app);
        }
        assert!(render(&app).contains("line-number-000"), "and back up");
    }

    #[test]
    fn ctrl_d_and_ctrl_u_jump_over_many_small_messages() {
        let mut app = App::new();
        let many: Vec<_> = (0..40)
            .map(|i| {
                msg(
                    &format!("m{i}"),
                    &format!("tiny-{i:02}"),
                    100 + i64::from(i),
                )
            })
            .collect();
        open_thread(&mut app, many);
        app.handle_key(Key::Char('o')); // 12 shown
        app.handle_key(Key::Char('g'));
        render(&app);
        let before = app.reader.as_ref().unwrap().selected;
        app.handle_key(Key::Ctrl('d'));
        render(&app);
        let after = app.reader.as_ref().unwrap().selected;
        assert!(
            after >= before + 2,
            "jumped several messages: {before} -> {after}"
        );
        app.handle_key(Key::Ctrl('u'));
        render(&app);
        assert!(app.reader.as_ref().unwrap().selected < after);
    }

    #[test]
    fn a_long_thread_shows_the_newest_six_and_o_loads_older_ones() {
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
        assert!(!text.contains("body-number-00"), "older ones are not shown");
        app.handle_key(Key::Char('g'));
        let text = render(&app);
        assert!(
            text.contains("body-number-34"),
            "g goes to the top of the shown ones"
        );
        assert!(!text.contains("body-number-00"));
        assert!(
            text.contains("34 older messages"),
            "a marker says what is hidden"
        );
        // `o` shows six more, from the stored copy
        app.handle_key(Key::Char('o'));
        app.handle_key(Key::Char('g'));
        let text = render(&app);
        assert!(text.contains("body-number-28") && text.contains("28 older messages"));
    }
}

/// One run of link text on screen that the terminal should make clickable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRun {
    pub x: u16,
    pub y: u16,
    pub url: String,
    /// The cells' symbols and background colours, left to right.
    pub cells: Vec<(String, Color)>,
}

/// Find the drawn link text (blue and underlined) that equals a shown link, and say which
/// address each run opens. Only addresses of the message text are ever returned.
#[must_use]
pub fn link_runs(buf: &ratatui::buffer::Buffer, links: &[(String, String)]) -> Vec<LinkRun> {
    let mut out = Vec::new();
    if links.is_empty() {
        return out;
    }
    let is_link = |c: &ratatui::buffer::Cell| {
        c.fg == theme::BLUE && c.modifier.contains(ratatui::style::Modifier::UNDERLINED)
    };
    for y in 0..buf.area.height {
        let mut x = 0;
        while x < buf.area.width {
            if !is_link(&buf[(x, y)]) {
                x += 1;
                continue;
            }
            let start = x;
            let mut shown = String::new();
            let mut cells = Vec::new();
            while x < buf.area.width && is_link(&buf[(x, y)]) {
                let cell = &buf[(x, y)];
                shown.push_str(cell.symbol());
                cells.push((cell.symbol().to_owned(), cell.bg));
                x += 1;
            }
            if let Some((_, url)) = links.iter().find(|(s, _)| *s == shown) {
                out.push(LinkRun {
                    x: start,
                    y,
                    url: url.clone(),
                    cells,
                });
            }
        }
    }
    out
}

/// The terminal commands that turn `runs` into OSC 8 hyperlinks: the same characters are
/// printed again, wrapped in the hyperlink start and end. The cursor is put back afterwards.
#[must_use]
pub fn link_escape(runs: &[LinkRun]) -> String {
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => format!("\x1b[48;2;{r};{g};{b}m"),
        _ => "\x1b[49m".to_owned(),
    };
    let Color::Rgb(r, g, b) = theme::BLUE else {
        return String::new();
    };
    let mut out = String::from("\x1b7");
    for run in runs {
        // never put a control character into the escape: only plain web addresses get here
        if run.url.chars().any(char::is_control) {
            continue;
        }
        out.push_str(&format!("\x1b[{};{}H", run.y + 1, run.x + 1));
        out.push_str(&format!(
            "\x1b]8;;{}\x1b\\\x1b[38;2;{r};{g};{b}m\x1b[4m",
            run.url
        ));
        for (symbol, bg) in &run.cells {
            out.push_str(&rgb(*bg));
            out.push_str(symbol);
        }
        out.push_str("\x1b]8;;\x1b\\\x1b[0m");
    }
    out.push_str("\x1b8");
    out
}

#[cfg(test)]
mod link_tests {
    use super::*;
    use ratatui::buffer::Buffer;

    #[test]
    fn drawn_link_text_becomes_a_hyperlink_to_the_full_address() {
        let area = Rect::new(0, 0, 30, 2);
        let mut buf = Buffer::empty(area);
        let link = Style::new()
            .fg(theme::BLUE)
            .add_modifier(Modifier::UNDERLINED);
        buf.set_string(2, 1, "https://a.org/…", link);
        buf.set_string(0, 0, "https://plain.org", Style::new());
        let links = vec![(
            "https://a.org/…".to_owned(),
            "https://a.org/full?x=1".to_owned(),
        )];
        let runs = link_runs(&buf, &links);
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].x, runs[0].y), (2, 1));
        assert_eq!(runs[0].url, "https://a.org/full?x=1");
        let escape = link_escape(&runs);
        assert!(
            escape.contains("\x1b]8;;https://a.org/full?x=1\x1b\\"),
            "{escape:?}"
        );
        assert!(escape.contains("\x1b]8;;\x1b\\"));
        assert!(escape.contains("\x1b[2;3H"), "{escape:?}");
        // blue text that is not a link of the message is left alone
        assert!(link_runs(&buf, &[]).is_empty());
    }
}
