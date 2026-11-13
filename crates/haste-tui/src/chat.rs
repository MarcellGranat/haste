//! The reader's chat layout: every message in a rounded frame (yours on the right), links
//! underlined, attachments as names, day separators. Pure: it turns messages into styled lines.

use haste_core::model::Message;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::inbox;
use crate::theme;

/// One laid-out message: its lines and the links found in its text.
#[derive(Debug, Clone)]
pub struct Laid {
    /// First line of the message's frame within the whole thread.
    pub start: usize,
    pub end: usize,
}

/// Everything the layout needs besides the messages.
#[derive(Debug, Clone, Copy)]
pub struct Layout<'a> {
    pub width: usize,
    pub selected: usize,
    /// The account's own address (mail), compared with the sender.
    pub own: Option<&'a str>,
    pub tz_offset: i64,
}

/// A link in a message: `http://` or `https://` up to whitespace, trailing punctuation dropped.
#[must_use]
pub fn links(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|w| {
            let w = w.trim_start_matches(['(', '<', '"', '\'', '[']);
            let w = w.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '>', '"', '\'', ']']);
            (w.starts_with("https://") || w.starts_with("http://"))
                .then(|| w.to_owned())
                .filter(|u| u.len() > 8 && !u.chars().any(char::is_control))
        })
        .collect()
}

/// An attachment as the stored text records it: iCloud writes
/// `[attachment: name (type, N bytes)]`, Beeper a bare `[attachment]` (no name is stored).
#[must_use]
pub fn attachment_of(line: &str) -> Option<String> {
    let line = line.trim();
    if line == "[attachment]" {
        return Some("attachment (name not stored)".to_owned());
    }
    let inner = line.strip_prefix("[attachment: ")?.strip_suffix(']')?;
    let (name, rest) = match inner.rsplit_once(" (") {
        Some((n, r)) => (n, r.trim_end_matches(')')),
        None => (inner, ""),
    };
    let mut parts = rest.split(", ");
    let kind = parts.next().unwrap_or("");
    let size = parts
        .next()
        .and_then(|s| s.strip_suffix(" bytes"))
        .and_then(|n| n.parse::<u64>().ok())
        .map(human_size);
    let mut out = name.to_owned();
    for extra in [Some(kind.to_owned()), size].into_iter().flatten() {
        if !extra.is_empty() {
            out.push_str(" · ");
            out.push_str(&extra);
        }
    }
    Some(out)
}

#[must_use]
pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KB", b / 1024),
        b => format!("{:.1} MB", b as f64 / 1_048_576.0),
    }
}

/// Split a message into its text and its attachment names.
#[must_use]
pub fn split_body(body: &str) -> (String, Vec<String>) {
    let mut text = Vec::new();
    let mut files = Vec::new();
    for line in body.lines() {
        match attachment_of(line) {
            Some(a) => files.push(a),
            None => text.push(line),
        }
    }
    (text.join("\n").trim().to_owned(), files)
}

/// One text line with its links underlined.
fn styled(line: &str, base: Style) -> Vec<Span<'static>> {
    let link = Style::new()
        .fg(theme::BLUE)
        .add_modifier(Modifier::UNDERLINED);
    let mut spans = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let at = rest
            .find("http://")
            .into_iter()
            .chain(rest.find("https://"))
            .min();
        match at {
            None => {
                spans.push(Span::styled(rest.to_owned(), base));
                break;
            }
            Some(i) => {
                if i > 0 {
                    spans.push(Span::styled(rest[..i].to_owned(), base));
                }
                let tail = &rest[i..];
                let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
                spans.push(Span::styled(tail[..end].to_owned(), link));
                rest = &tail[end..];
            }
        }
    }
    spans
}

fn is_own(m: &Message, own: Option<&str>) -> bool {
    m.is_own
        || matches!((own, m.sender_identifier.as_deref()),
            (Some(o), Some(s)) if s.to_lowercase().contains(&o.to_lowercase()))
}

/// Lay out the thread. Returns the lines and where each message sits.
#[must_use]
pub fn layout(messages: &[Message], l: &Layout<'_>) -> (Vec<Line<'static>>, Vec<Laid>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut laid = Vec::new();
    let max_inner = (l.width * 3 / 4).clamp(16, 100).saturating_sub(4);
    let mut day = None;
    for (i, m) in messages.iter().enumerate() {
        let this_day = inbox::day_start(m.sent_at, l.tz_offset);
        if day != Some(this_day) {
            day = Some(this_day);
            let label = format!("── {} ──", inbox::day_label(m.sent_at, l.tz_offset));
            let lead = l.width.saturating_sub(label.chars().count()) / 2;
            lines.push(Line::from(Span::styled(
                format!("{}{label}", " ".repeat(lead)),
                theme::muted(),
            )));
        }
        let own = is_own(m, l.own);
        let selected = i == l.selected;
        let (text, files) = split_body(&inbox::sanitize(&m.body_text));
        let mut body: Vec<String> = if text.is_empty() && files.is_empty() {
            vec!["(no text)".to_owned()]
        } else {
            inbox::wrap(&text, max_inner)
        };
        if text.is_empty() {
            body.clear();
        }
        let chips: Vec<String> = files.iter().map(|f| format!("◫ {f}")).collect();
        let who = if own {
            "you".to_owned()
        } else {
            inbox::sanitize(m.sender_identifier.as_deref().unwrap_or("unknown sender"))
                .replace('\n', " ")
        };
        let header = format!(" {who} · {} ", inbox::stamp(m.sent_at, l.tz_offset));
        let inner = body
            .iter()
            .map(|s| s.chars().count())
            .chain(chips.iter().map(|s| s.chars().count()))
            .chain([header.chars().count().saturating_sub(2)])
            .max()
            .unwrap_or(10)
            .clamp(10, max_inner.max(10));
        let frame_w = inner + 4;
        let indent = if own {
            " ".repeat(l.width.saturating_sub(frame_w))
        } else {
            String::new()
        };
        let colour = if selected {
            theme::MAUVE
        } else if own {
            theme::TEAL
        } else {
            theme::BORDER
        };
        let edge = Style::new().fg(colour);
        let head_style = Style::new()
            .fg(if own { theme::TEAL } else { theme::LAVENDER })
            .add_modifier(Modifier::BOLD);
        let start = lines.len();
        // top edge: ╭─ who · time ───╮
        let used = header.chars().count() + 1;
        lines.push(Line::from(vec![
            Span::raw(indent.clone()),
            Span::styled("╭─", edge),
            Span::styled(header, head_style),
            Span::styled(
                format!("{}╮", "─".repeat(frame_w.saturating_sub(used + 2))),
                edge,
            ),
        ]));
        let row = |content: Vec<Span<'static>>, len: usize| {
            let mut spans = vec![Span::raw(indent.clone()), Span::styled("│ ", edge)];
            spans.extend(content);
            spans.push(Span::raw(" ".repeat(inner.saturating_sub(len))));
            spans.push(Span::styled(" │", edge));
            Line::from(spans)
        };
        for b in &body {
            let len = b.chars().count();
            lines.push(row(styled(b, Style::new().fg(theme::TEXT)), len));
        }
        for c in &chips {
            let len = c.chars().count();
            lines.push(row(
                vec![Span::styled(c.clone(), Style::new().fg(theme::PEACH))],
                len,
            ));
        }
        lines.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(format!("╰{}╯", "─".repeat(frame_w - 2)), edge),
        ]));
        laid.push(Laid {
            start,
            end: lines.len(),
        });
    }
    (lines, laid)
}

/// The scroll offset that keeps the selected message in view, moving as little as possible.
#[must_use]
pub fn scroll_to(current: usize, laid: &Laid, height: usize, total: usize) -> usize {
    let mut s = current;
    if laid.end > s + height {
        s = laid.end.saturating_sub(height);
    }
    if laid.start < s {
        s = laid.start;
    }
    s.min(total.saturating_sub(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, who: &str, text: &str, at: i64, own: bool) -> Message {
        Message {
            id: id.into(),
            thread_id: "t".into(),
            source: haste_core::model::Source::Whatsapp,
            external_id: id.into(),
            sender_person_id: None,
            sender_identifier: Some(who.into()),
            body_text: text.into(),
            sent_at: at,
            is_own: own,
        }
    }

    fn text_of(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn links_are_found_and_trimmed() {
        assert_eq!(
            links("see (https://a.org/x?y=1), and http://b.io."),
            ["https://a.org/x?y=1", "http://b.io"]
        );
        assert!(links("ftp://nope javascript:alert(1) https://").is_empty());
    }

    #[test]
    fn attachments_are_read_from_the_stored_text() {
        assert_eq!(
            attachment_of("[attachment: Q3 report.pdf (application/pdf, 2048 bytes)]").unwrap(),
            "Q3 report.pdf · application/pdf · 2 KB"
        );
        assert_eq!(
            attachment_of("[attachment]").unwrap(),
            "attachment (name not stored)"
        );
        assert!(attachment_of("an ordinary line").is_none());
        let (text, files) = split_body("hello\n[attachment: a.png (image/png, 10 bytes)]");
        assert_eq!((text.as_str(), files.len()), ("hello", 1));
    }

    #[test]
    fn frames_are_aligned_and_your_messages_sit_on_the_right() {
        let msgs = [
            m(
                "1",
                "Ana",
                "hello there https://x.org/a",
                1_700_000_000,
                false,
            ),
            m(
                "2",
                "me",
                "hi\n[attachment: f.pdf (application/pdf, 5 bytes)]",
                1_700_000_100,
                true,
            ),
        ];
        let (lines, laid) = layout(
            &msgs,
            &Layout {
                width: 60,
                selected: 0,
                own: None,
                tz_offset: 0,
            },
        );
        assert_eq!(laid.len(), 2);
        let first = &lines[laid[0].start];
        assert!(text_of(first).starts_with("╭─ Ana"));
        // every row of a frame has the same width
        for l in &lines[laid[0].start..laid[0].end] {
            assert_eq!(text_of(l).chars().count(), text_of(first).chars().count());
        }
        let mine = text_of(&lines[laid[1].start]);
        assert!(mine.starts_with(' '), "yours is indented to the right");
        assert!(mine.trim_start().starts_with("╭─ you"));
        assert!(lines[laid[1].start..laid[1].end]
            .iter()
            .any(|l| text_of(l).contains("◫ f.pdf")));
        // the link is underlined
        assert!(lines[laid[0].start + 1]
            .spans
            .iter()
            .any(|s| s.content.starts_with("https://")
                && s.style.add_modifier.contains(Modifier::UNDERLINED)));
    }

    #[test]
    fn scrolling_keeps_the_selected_message_visible() {
        let laid = Laid { start: 30, end: 40 };
        assert_eq!(scroll_to(0, &laid, 20, 100), 20);
        assert_eq!(scroll_to(35, &laid, 20, 100), 30);
        assert_eq!(scroll_to(25, &laid, 20, 100), 25);
    }
}
