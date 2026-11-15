//! The reader's chat layout: every message in a rounded frame (yours on the right), links
//! underlined, attachments as names, day separators. Pure: it turns messages into styled lines.

use haste_core::model::Message;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::inbox;
use crate::theme;

/// A recipient list longer than this is collapsed until the message is expanded.
pub const RECIPIENTS_COLLAPSE_AT: usize = 120;
/// How much of a collapsed recipient list stays visible.
const RECIPIENTS_SHOWN: usize = 90;

/// Turn the HTML entities mail leaves in plain text back into characters.
#[must_use]
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').filter(|&e| e <= 10).and_then(|e| {
            let name = &rest[1..e];
            let ch = match name {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                _ => name
                    .strip_prefix('#')
                    .and_then(|n| {
                        n.strip_prefix(['x', 'X'])
                            .map_or_else(|| n.parse().ok(), |h| u32::from_str_radix(h, 16).ok())
                    })
                    .and_then(char::from_u32),
            };
            ch.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A piece of a message body after folding: text to show, or a line saying what is folded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    Fold(String),
}

fn is_quote(line: &str) -> bool {
    line.trim_start().starts_with('>')
}

fn is_wrote(line: &str) -> bool {
    let t = line.trim_end();
    ["wrote:", "írta:", "a écrit :", "a écrit:", "schrieb:"]
        .iter()
        .any(|s| t.ends_with(s))
}

fn is_header_block_start(lines: &[&str], i: usize) -> bool {
    let t = lines[i].trim();
    if t.starts_with("-----Original Message") || t.starts_with("-----Eredeti üzenet") {
        return true;
    }
    t.starts_with("From:")
        && lines[i + 1..lines.len().min(i + 5)].iter().any(|l| {
            let l = l.trim_start();
            l.starts_with("Sent:") || l.starts_with("Date:") || l.starts_with("To:")
        })
}

/// The quoted history in `lines`: `(start, end, to_the_end)` blocks, in order. A block of `>`
/// lines (with its "wrote:" line) ends where the quote ends; an "Original Message" or
/// "From:/Sent:" header block runs to the end. A block at the very top is the message itself
/// (a forward), not history.
fn history_blocks(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if is_quote(lines[i]) {
            let mut start = i;
            if start > 0 && is_wrote(lines[start - 1]) {
                start -= 1;
                if start > 0 && lines[start - 1].trim_start().starts_with("On ") {
                    start -= 1;
                }
            }
            let mut end = i;
            let mut j = i;
            while j < lines.len() {
                if is_quote(lines[j]) {
                    j += 1;
                    end = j;
                } else if lines[j].trim().is_empty() {
                    j += 1;
                } else {
                    break;
                }
            }
            blocks.push((start, end));
            i = end.max(i + 1);
        } else if i > 0 && is_header_block_start(lines, i) {
            blocks.push((i, lines.len()));
            break;
        } else {
            i += 1;
        }
    }
    blocks
}

/// Collapse a long `To:`/`Cc:` list (with its continuation lines) into one short line.
fn fold_recipients(lines: &[String], expanded: bool) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let lower = lines[i].trim_start().to_lowercase();
        let is_list = ["to:", "cc:", "bcc:"].iter().any(|k| lower.starts_with(k));
        if !is_list {
            out.push(Piece::Text(lines[i].clone()));
            i += 1;
            continue;
        }
        let mut joined = lines[i].trim().to_owned();
        let mut j = i + 1;
        // continuation lines: not blank, not another `Key:` header, not a quote
        while j < lines.len() {
            let l = lines[j].trim();
            let header_like = l
                .split_once(':')
                .is_some_and(|(k, _)| !k.is_empty() && k.len() < 16 && !k.contains(' '));
            if l.is_empty() || header_like || is_quote(l) {
                break;
            }
            joined.push(' ');
            joined.push_str(l);
            j += 1;
        }
        if expanded || inbox_width(&joined) <= RECIPIENTS_COLLAPSE_AT {
            out.extend(lines[i..j].iter().cloned().map(Piece::Text));
        } else {
            let mut cut = joined
                .char_indices()
                .nth(RECIPIENTS_SHOWN)
                .map_or(joined.len(), |(b, _)| b);
            // end on a separator so no address is cut in half
            if let Some(p) = joined[..cut].rfind(';') {
                cut = p + 1;
            }
            let more = joined[cut..].matches('@').count();
            out.push(Piece::Text(format!("{} …", joined[..cut].trim_end())));
            out.push(Piece::Fold(format!(
                "▸ +{more} more recipients (x to show)"
            )));
        }
        i = j;
    }
    out
}

fn inbox_width(s: &str) -> usize {
    inbox::width(s)
}

/// Fold a message's text: quoted history becomes one line, a long recipient list is shortened.
/// `expanded` shows everything. A message that is all history (a forward) is not hidden.
#[must_use]
pub fn fold(text: &str, expanded: bool) -> Vec<Piece> {
    let lines: Vec<&str> = text.lines().collect();
    let blocks = if expanded {
        Vec::new()
    } else {
        history_blocks(&lines)
    };
    let hidden: usize = blocks.iter().map(|(s, e)| e - s).sum();
    let blocks = if hidden >= lines.len() {
        Vec::new()
    } else {
        blocks
    };
    let mut out = Vec::new();
    let mut at = 0;
    let mut visible: Vec<String> = Vec::new();
    let flush = |visible: &mut Vec<String>, out: &mut Vec<Piece>| {
        out.extend(fold_recipients(visible, expanded));
        visible.clear();
    };
    for (start, end) in blocks {
        visible.extend(lines[at..start].iter().map(|l| (*l).to_owned()));
        flush(&mut visible, &mut out);
        let n = lines[start..end]
            .iter()
            .filter(|l| !l.trim().is_empty())
            .count();
        out.push(Piece::Fold(format!("▸ {n} quoted lines (x to show)")));
        at = end;
    }
    visible.extend(lines[at..].iter().map(|l| (*l).to_owned()));
    flush(&mut visible, &mut out);
    // trim blank edges
    while matches!(out.first(), Some(Piece::Text(t)) if t.trim().is_empty()) {
        out.remove(0);
    }
    while matches!(out.last(), Some(Piece::Text(t)) if t.trim().is_empty()) {
        out.pop();
    }
    out
}

/// One laid-out message: its lines and the links found in its text.
#[derive(Debug, Clone)]
pub struct Laid {
    /// First line of the message's frame within the whole thread.
    pub start: usize,
    pub end: usize,
    /// `(shown text, full address)` for every link in the message. A long address is shown
    /// shortened; the full one is what a click opens.
    pub links: Vec<(String, String)>,
}

/// Everything the layout needs besides the messages.
#[derive(Debug, Clone, Copy)]
pub struct Layout<'a> {
    pub width: usize,
    pub selected: usize,
    /// The account's own address (mail), compared with the sender.
    pub own: Option<&'a str>,
    pub tz_offset: i64,
    /// The current time (Unix seconds), for recent times such as `3 hours ago`.
    pub now: i64,
    /// How many older messages are not laid out (a marker line says so).
    pub hidden: usize,
    /// Messages (by id) shown in full, with their quoted history and recipient lists open.
    pub expanded: &'a std::collections::HashSet<String>,
}

/// Trim a word to the web address inside it: leading brackets and trailing punctuation dropped.
fn web_address(word: &str) -> Option<&str> {
    let w = word.trim_start_matches(['(', '<', '"', '\'', '[']);
    let w = w.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '>', '"', '\'', ']']);
    (w.starts_with("https://") || w.starts_with("http://"))
        .then_some(w)
        .filter(|u| u.len() > 8 && !u.chars().any(char::is_control))
}

/// A link in a message: `http://` or `https://` up to whitespace, trailing punctuation dropped.
#[must_use]
pub fn links(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|w| web_address(w).map(str::to_owned))
        .collect()
}

/// Longest address shown in full; longer ones are shown as `https://host/…`.
const LINK_SHOWN: usize = 40;

/// What is shown for an address: itself when short, else its scheme and host with `/…`, cut to
/// `max` columns. Two different addresses never share a shown text (a number is added).
fn shown_link(url: &str, max: usize, taken: &[(String, String)]) -> String {
    let max = max.max(12);
    let base = if url.chars().count() <= LINK_SHOWN.min(max) {
        url.to_owned()
    } else {
        let host_end = url.find("://").map_or(url.len(), |i| {
            url[i + 3..]
                .find(['/', '?', '#'])
                .map_or(url.len(), |j| i + 3 + j)
        });
        let host: String = url[..host_end]
            .chars()
            .take(max.saturating_sub(2))
            .collect();
        format!("{host}/…")
    };
    let mut shown = base.clone();
    let mut n = 1;
    while taken.iter().any(|(s, u)| *s == shown && u != url) {
        n += 1;
        shown = format!("{base}{n}");
    }
    shown
}

/// Replace every long address in `text` by its shown form, recording the pairs in `out`.
fn shorten_links(text: &str, max: usize, out: &mut Vec<(String, String)>) -> String {
    let mut result = String::with_capacity(text.len());
    let mut word = String::new();
    let mut flush = |word: &mut String, result: &mut String| {
        if let Some(url) = web_address(word) {
            let at = word.find(url).unwrap_or(0);
            let shown = shown_link(url, max, out);
            if !out.iter().any(|(_, u)| u == url) {
                out.push((shown.clone(), url.to_owned()));
            }
            let shown = out
                .iter()
                .find(|(_, u)| u == url)
                .map_or(shown, |(s, _)| s.clone());
            result.push_str(&word[..at]);
            result.push_str(&shown);
            result.push_str(&word[at + url.len()..]);
        } else {
            result.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_whitespace() {
            flush(&mut word, &mut result);
            result.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut result);
    result
}

/// An attachment line as the reader shows it: `name · type · size`. Older Beeper rows are a
/// bare `[attachment]` with no name.
#[must_use]
pub fn attachment_of(line: &str) -> Option<String> {
    if line.trim() == "[attachment]" {
        return Some("attachment (name not stored)".to_owned());
    }
    let a = haste_core::attachment::parse_line(line)?;
    let mut out = a.name;
    if !a.mime.is_empty() {
        out.push_str(" · ");
        out.push_str(&a.mime);
    }
    if a.bytes > 0 {
        out.push_str(" · ");
        out.push_str(&human_size(a.bytes));
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
                let word_end = tail.find(char::is_whitespace).unwrap_or(tail.len());
                let end = web_address(&tail[..word_end]).map_or(word_end, str::len);
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

/// Longest gap (seconds) between two attachment-only messages that still count as one batch.
const BATCH_GAP: i64 = 300;

/// A message that is nothing but attachments (and no reactions yet on it).
fn only_files(m: &Message) -> bool {
    let (plain, _) = haste_core::reactions::take(&m.body_text);
    let (text, files) = split_body(&plain);
    text.is_empty() && !files.is_empty()
}

/// Does `cur` continue the batch that `prev` belongs to? Same sender, both attachment-only,
/// close in time, and nothing reacted to on `prev` (a reaction belongs to its own message).
fn joins_batch(prev: &Message, cur: &Message) -> bool {
    prev.is_own == cur.is_own
        && prev.sender_identifier == cur.sender_identifier
        && (cur.sent_at - prev.sent_at).abs() <= BATCH_GAP
        && only_files(prev)
        && only_files(cur)
        && haste_core::reactions::take(&prev.body_text).1.is_none()
}

/// The last message of the batch that starts at or contains `i`.
#[must_use]
pub fn batch_end(messages: &[Message], i: usize) -> usize {
    let mut end = i;
    while end + 1 < messages.len() && joins_batch(&messages[end], &messages[end + 1]) {
        end += 1;
    }
    end
}

/// The first message of the batch containing `i`.
#[must_use]
pub fn batch_start(messages: &[Message], i: usize) -> usize {
    let mut start = i.min(messages.len().saturating_sub(1));
    while start > 0 && joins_batch(&messages[start - 1], &messages[start]) {
        start -= 1;
    }
    start
}

/// Lay out the thread. Returns the lines and where each message sits.
#[must_use]
pub fn layout(messages: &[Message], l: &Layout<'_>) -> (Vec<Line<'static>>, Vec<Laid>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut laid = Vec::new();
    let max_inner = (l.width * 3 / 4).clamp(16, 100).saturating_sub(4);
    let mut day = None;
    if l.hidden > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "▲ {} older message{} · o shows {} more",
                l.hidden,
                if l.hidden == 1 { "" } else { "s" },
                crate::app::READER_WINDOW
            ),
            Style::new().fg(theme::LAVENDER),
        )));
    }
    let mut i = 0;
    while i < messages.len() {
        let end = batch_end(messages, i);
        let m = &messages[i];
        let this_day = inbox::day_start(m.sent_at, l.tz_offset);
        if day != Some(this_day) {
            day = Some(this_day);
            let label = format!("── {} ──", inbox::day_label(m.sent_at, l.tz_offset));
            let lead = l.width.saturating_sub(inbox::width(&label)) / 2;
            lines.push(Line::from(Span::styled(
                format!("{}{label}", " ".repeat(lead)),
                theme::muted(),
            )));
        }
        let own = is_own(m, l.own);
        let selected = (i..=end).contains(&l.selected);
        let (plain, _) = haste_core::reactions::take(&m.body_text);
        let (text, mut files) = split_body(&inbox::sanitize(&decode_entities(&plain)));
        for extra in &messages[i + 1..=end] {
            let (p, _) = haste_core::reactions::take(&extra.body_text);
            files.extend(split_body(&inbox::sanitize(&decode_entities(&p))).1);
        }
        let reactions = haste_core::reactions::take(&messages[end].body_text).1;
        let reactions = reactions.map(|r| format!("↳ {}", inbox::sanitize(&r).replace('\n', " ")));
        let pieces = fold(&text, l.expanded.contains(&m.id));
        // each row: the text, and whether it is a fold marker
        let mut body: Vec<(String, bool)> = Vec::new();
        let mut message_links: Vec<(String, String)> = Vec::new();
        for piece in &pieces {
            match piece {
                Piece::Text(t) => {
                    let t = shorten_links(t, max_inner, &mut message_links);
                    body.extend(inbox::wrap(&t, max_inner).into_iter().map(|s| (s, false)))
                }
                Piece::Fold(t) => body.push((t.clone(), true)),
            }
        }
        if body.is_empty() && files.is_empty() {
            body.push(("(no text)".to_owned(), false));
        }
        let chips: Vec<String> = files.iter().map(|f| format!("◫ {f}")).collect();
        let who = if own {
            "you".to_owned()
        } else if inbox::is_mail(&m.source.as_name()) {
            inbox::short_sender_or_address(
                &inbox::sanitize(m.sender_identifier.as_deref().unwrap_or("unknown sender"))
                    .replace('\n', " "),
            )
        } else {
            inbox::sanitize(m.sender_identifier.as_deref().unwrap_or("unknown sender"))
                .replace('\n', " ")
        };
        let count = if end > i {
            format!("{} files · ", files.len())
        } else {
            String::new()
        };
        let header = format!(
            " {who} · {count}{} ",
            inbox::smart_stamp(m.sent_at, l.now, l.tz_offset)
        );
        let inner = body
            .iter()
            .map(|(s, _)| inbox::width(s))
            .chain(chips.iter().map(|s| inbox::width(s)))
            .chain(reactions.iter().map(|s| inbox::width(s)))
            .chain([inbox::width(&header).saturating_sub(2)])
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
        let used = inbox::width(&header) + 1;
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
        for (b, fold_line) in &body {
            let len = inbox::width(b);
            let content = if *fold_line {
                vec![Span::styled(b.clone(), Style::new().fg(theme::LAVENDER))]
            } else {
                styled(b, Style::new().fg(theme::TEXT))
            };
            lines.push(row(content, len));
        }
        if let Some(r) = &reactions {
            let len = inbox::width(r);
            lines.push(row(
                vec![Span::styled(r.clone(), Style::new().fg(theme::YELLOW))],
                len,
            ));
        }
        for c in &chips {
            let len = inbox::width(c);
            lines.push(row(
                vec![Span::styled(c.clone(), Style::new().fg(theme::PEACH))],
                len,
            ));
        }
        lines.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(format!("╰{}╯", "─".repeat(frame_w - 2)), edge),
        ]));
        // every message of a batch shares the one frame
        for _ in i..=end {
            laid.push(Laid {
                start,
                end: lines.len(),
                links: message_links.clone(),
            });
        }
        i = end + 1;
    }
    (lines, laid)
}

/// The scroll offset that keeps the selected message in view, moving as little as possible.
#[must_use]
pub fn scroll_to(current: usize, laid: &Laid, height: usize, total: usize) -> usize {
    if laid.end.saturating_sub(laid.start) > height {
        // taller than the screen: any position inside it is fine (Ctrl-d and Ctrl-u read it)
        return current
            .clamp(laid.start, laid.end - height)
            .min(total.saturating_sub(height));
    }
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
    fn reactions_are_shown_under_the_message() {
        let messages = [m(
            "1",
            "Ana",
            "great news\n[reactions: 👍×2 ❤️]",
            1_700_000_000,
            false,
        )];
        let (lines, laid) = layout(
            &messages,
            &Layout {
                width: 60,
                selected: 0,
                own: None,
                tz_offset: 0,
                now: 0,
                hidden: 0,
                expanded: &std::collections::HashSet::new(),
            },
        );
        let all: Vec<String> = lines[laid[0].start..laid[0].end]
            .iter()
            .map(text_of)
            .collect();
        assert!(all.iter().any(|l| l.contains("great news")), "{all:?}");
        assert!(all.iter().any(|l| l.contains("↳ 👍×2 ❤️")), "{all:?}");
        assert!(!all.iter().any(|l| l.contains("[reactions")));
    }

    #[test]
    fn long_addresses_are_shown_short_and_keep_their_full_form() {
        let long =
            "https://click.example.com/ls/click?upn=u001.AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let other =
            "https://click.example.com/ls/click?upn=u001.ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ";
        let mut map = Vec::new();
        let text = format!("go ({long}), or {other}. short: https://a.org/x and {long} again");
        let shown = shorten_links(&text, 60, &mut map);
        assert_eq!(
            shown,
            "go (https://click.example.com/…), or https://click.example.com/…2. \
             short: https://a.org/x and https://click.example.com/… again"
        );
        assert_eq!(
            map,
            vec![
                ("https://click.example.com/…".to_owned(), long.to_owned()),
                ("https://click.example.com/…2".to_owned(), other.to_owned()),
                ("https://a.org/x".to_owned(), "https://a.org/x".to_owned()),
            ]
        );
    }

    #[test]
    fn the_layout_records_the_links_of_each_message() {
        let long =
            "https://click.example.com/ls/click?upn=u001.AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let messages = [m(
            "m1",
            "Ana",
            &format!("Aktiválás ({long})"),
            1_700_000_000,
            false,
        )];
        let (lines, laid) = layout(
            &messages,
            &Layout {
                width: 60,
                selected: 0,
                own: None,
                tz_offset: 0,
                now: 0,
                hidden: 0,
                expanded: &std::collections::HashSet::new(),
            },
        );
        assert_eq!(
            laid[0].links,
            vec![("https://click.example.com/…".to_owned(), long.to_owned())]
        );
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("https://click.example.com/…") && !text.contains("upn="));
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
    fn a_run_of_image_messages_is_one_frame() {
        let img = "[attachment: image.jpg (image/jpeg, 10 bytes)]";
        let messages = vec![
            m("a", "me", "hello", 100, true),
            m("b", "me", img, 200, true),
            m("c", "me", img, 205, true),
            m("d", "me", img, 210, true),
            m("e", "you", img, 215, false),
        ];
        assert_eq!(batch_end(&messages, 1), 3);
        assert_eq!(batch_start(&messages, 3), 1);
        assert_eq!(batch_end(&messages, 0), 0, "a text message starts no batch");
        assert_eq!(batch_end(&messages, 4), 4, "another sender is separate");
        let expanded = std::collections::HashSet::new();
        let l = Layout {
            width: 80,
            selected: 2,
            own: None,
            tz_offset: 0,
            now: 300,
            hidden: 0,
            expanded: &expanded,
        };
        let (lines, laid) = layout(&messages, &l);
        assert_eq!(laid.len(), messages.len());
        assert_eq!((laid[1].start, laid[1].end), (laid[3].start, laid[3].end));
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text.iter().filter(|t| t.contains("image.jpg")).count(), 4);
        assert_eq!(text.iter().filter(|t| t.contains("╭─")).count(), 3);
        assert!(text.iter().any(|t| t.contains("3 files")));
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
                now: 0,
                hidden: 0,
                expanded: &std::collections::HashSet::new(),
            },
        );
        assert_eq!(laid.len(), 2);
        let first = &lines[laid[0].start];
        assert!(text_of(first).starts_with("╭─ Ana"));
        // every row of a frame has the same width
        for l in &lines[laid[0].start..laid[0].end] {
            assert_eq!(inbox::width(&text_of(l)), inbox::width(&text_of(first)));
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
    fn entities_are_decoded() {
        assert_eq!(
            decode_entities("&lt;your message&gt; &amp; &#39;x&#39; &#x41;"),
            "<your message> & 'x' A"
        );
        assert_eq!(decode_entities("a & b &unknown; c"), "a & b &unknown; c");
    }

    fn texts(pieces: &[Piece]) -> Vec<String> {
        pieces
            .iter()
            .map(|p| match p {
                Piece::Text(t) | Piece::Fold(t) => t.clone(),
            })
            .collect()
    }

    #[test]
    fn quoted_history_is_folded_and_opens() {
        let body = "Igen, ezt csináltam.\n\nOn Thu, Nov 12, 2026 at 8:03 AM Gábor <g@x.hu> wrote:\n\n> Szia Marcell,\n> Tudnál küldeni?\n>\n> Gábor";
        let folded = texts(&fold(body, false));
        assert_eq!(folded[0], "Igen, ezt csináltam.");
        assert!(
            folded.last().unwrap().starts_with("▸ 4 quoted lines"),
            "{folded:?}"
        );
        assert!(!folded.iter().any(|l| l.contains("Szia Marcell")));
        let open = texts(&fold(body, true));
        assert!(open.iter().any(|l| l.contains("> Szia Marcell,")));
        // an inline answer after the quote stays visible
        let inline = texts(&fold("> question\n\nmy answer", false));
        assert_eq!(inline.last().unwrap(), "my answer");
    }

    #[test]
    fn a_long_recipient_list_is_collapsed_until_opened() {
        let list: Vec<String> = (0..40)
            .map(|i| format!("Person {i} <person{i}@gtk.elte.hu>"))
            .collect();
        let body = format!(
            "From: Dean <dekan@gtk.elte.hu>\nSent: Monday\nTo: {}\nSubject: Hello\n\nBody text",
            list.join("; ")
        );
        let folded = texts(&fold(&body, false));
        assert!(folded.iter().any(|l| l.starts_with("To: Person 0")));
        assert!(
            folded.iter().any(|l| l.contains("more recipients")),
            "{folded:?}"
        );
        assert!(folded.iter().all(|l| inbox::width(l) < 140));
        assert!(folded.contains(&"Subject: Hello".to_owned()));
        let open = texts(&fold(&body, true));
        assert!(open.iter().any(|l| l.contains("person39@")));
    }

    #[test]
    fn frames_stay_aligned_with_wide_and_zero_width_characters() {
        let msgs = [m(
            "1",
            "Ana",
            "漢字 and a\u{200b}zero width, é\u{0301} accents",
            1_700_000_000,
            false,
        )];
        let (lines, laid) = layout(
            &msgs,
            &Layout {
                width: 50,
                selected: 0,
                own: None,
                tz_offset: 0,
                now: 0,
                hidden: 0,
                expanded: &std::collections::HashSet::new(),
            },
        );
        let w = inbox::width(&text_of(&lines[laid[0].start]));
        for l in &lines[laid[0].start..laid[0].end] {
            assert_eq!(inbox::width(&text_of(l)), w, "{}", text_of(l));
        }
    }

    #[test]
    fn scrolling_keeps_the_selected_message_visible() {
        let laid = Laid {
            start: 30,
            end: 40,
            links: Vec::new(),
        };
        assert_eq!(scroll_to(0, &laid, 20, 100), 20);
        assert_eq!(scroll_to(35, &laid, 20, 100), 30);
        assert_eq!(scroll_to(25, &laid, 20, 100), 25);
        // a message taller than the screen can be scrolled through
        let tall = Laid {
            start: 10,
            end: 70,
            links: Vec::new(),
        };
        assert_eq!(
            scroll_to(0, &tall, 20, 100),
            10,
            "arriving from above: its top"
        );
        assert_eq!(scroll_to(30, &tall, 20, 100), 30);
        assert_eq!(scroll_to(90, &tall, 20, 100), 50, "its bottom, not past it");
    }
}
