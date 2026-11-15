//! Pure helpers for the inbox tabs: filter matching, due-date and snooze prompts, day windows.

use haste_core::time::{from_civil, to_civil};
use haste_proto::{FilterView, ThreadView};

/// Channels shown on the Channels tab, in order.
pub const CHANNELS: [&str; 5] = ["gmail", "icloud", "whatsapp", "messenger", "imessage"];
/// How far ahead the Calendar tab looks.
pub const AGENDA_DAYS: i64 = 7;
/// Longest snooze accepted.
pub const MAX_SNOOZE_DAYS: i64 = 365;
/// Hour of day (local time) used for "morning" and for a reminder given only a date.
pub const DEFAULT_REMINDER_HOUR: i64 = 9;
/// Rows of the inbox pane on the Today tab; longer lists scroll.
pub const INBOX_ROWS: usize = 25;
/// How far ahead "later" is.
pub const LATER_SECS: i64 = 3 * HOUR;
const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;

/// Mail channels, where a reply can address several people.
#[must_use]
pub fn is_mail(source: &str) -> bool {
    matches!(source, "gmail" | "icloud")
}

/// Local parts that name no one (a robot address): the domain says more.
const ROBOT_LOCAL_PARTS: [&str; 12] = [
    "noreply",
    "no-reply",
    "no_reply",
    "donotreply",
    "do-not-reply",
    "do_not_reply",
    "notification",
    "notifications",
    "notify",
    "mailer-daemon",
    "postmaster",
    "bounce",
];

/// Is this local part a robot's (`noreply`, `no-reply-qwrphp-k3x…`, `notifications+abc`)? It is
/// when it is one of the names or starts with one followed by a separator.
fn is_robot_local(local: &str) -> bool {
    let local = local.to_lowercase();
    ROBOT_LOCAL_PARTS.iter().any(|p| {
        local == *p
            || local
                .strip_prefix(p)
                .and_then(|rest| rest.chars().next())
                .is_some_and(|c| matches!(c, '-' | '_' | '.' | '+'))
    })
}

/// The name of an address's domain without its ending: `accounts.google.com` is `google`,
/// `news.shop.co.uk` is `shop`. Empty when the address has no domain.
fn domain_name(address: &str) -> String {
    let domain = address.rsplit_once('@').map_or("", |(_, d)| d).trim();
    let mut labels: Vec<&str> = domain.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() > 1 {
        let second_level = matches!(
            labels[labels.len() - 2],
            "co" | "com" | "org" | "net" | "ac" | "gov"
        ) && labels[labels.len() - 1].len() == 2;
        labels.truncate(
            labels.len()
                - if second_level && labels.len() > 2 {
                    2
                } else {
                    1
                },
        );
    }
    labels.last().map(|l| (*l).to_owned()).unwrap_or_default()
}

/// A sender for a list row or a message header: the display name when the identifier is
/// `Name <addr>`; for a robot address (`noreply@…`) the domain without its ending; else the
/// part before the `@`, else the whole identifier.
#[must_use]
pub fn short_sender(id: &str) -> String {
    let id = id.trim();
    if let Some((name, _)) = id.split_once('<') {
        let name = name.trim().trim_matches('"');
        if !name.is_empty() {
            return name.to_owned();
        }
    }
    let bare = id.trim_matches(|c| c == '<' || c == '>');
    let local = bare.split('@').next().unwrap_or(bare);
    if bare.contains('@') && is_robot_local(local) {
        let domain = domain_name(bare);
        if !domain.is_empty() {
            return domain;
        }
    }
    local.to_owned()
}

/// A message header's sender: the domain name for a robot address, otherwise the identifier as
/// it is (the full address, so it can be told apart).
#[must_use]
pub fn short_sender_or_address(id: &str) -> String {
    let short = short_sender(id);
    let bare = id.trim().trim_matches(|c| c == '<' || c == '>');
    let local = bare.split('@').next().unwrap_or(bare);
    if bare.contains('@') && is_robot_local(local) {
        short
    } else {
        id.to_owned()
    }
}

/// What the mute list is matched against: the thread's senders, and for a chat also its name.
#[must_use]
pub fn senders(t: &ThreadView) -> Vec<&str> {
    let mut out: Vec<&str> = t.thread.participants.iter().map(String::as_str).collect();
    if !is_mail(&t.thread.source.as_name()) {
        out.push(t.thread.subject.as_str());
    }
    out
}

/// What the sender rules (mute, priority, allowed) are matched against: for mail the newest
/// sender who is not the human, else the participants; for chats the chat name and people.
#[must_use]
pub fn rule_subjects(t: &ThreadView) -> Vec<&str> {
    if is_mail(&t.thread.source.as_name()) {
        if let Some(s) = t.sender.as_deref() {
            return vec![s];
        }
    }
    senders(t)
}

#[must_use]
pub fn matches(filter: &FilterView, t: &ThreadView) -> bool {
    let lower = |s: &str| s.to_lowercase();
    let any = |needles: &[String], hay: &str| {
        needles.is_empty() || needles.iter().any(|n| lower(hay).contains(&lower(n)))
    };
    let sender_ok =
        filter.sender.is_empty() || t.thread.participants.iter().any(|p| any(&filter.sender, p));
    let source_ok = filter.source.is_empty()
        || filter
            .source
            .iter()
            .any(|s| lower(s) == t.thread.source.as_name());
    sender_ok
        && source_ok
        && any(&filter.subject, &t.thread.subject)
        && filter.min_priority.is_none_or(|m| t.triage.priority >= m)
}

/// Seconds from a snooze prompt: whole hours or whole days (`3h`, `2d`).
///
/// # Errors
/// A short message for anything else (fractions, zero, other units, too long).
pub fn parse_snooze(input: &str) -> Result<i64, String> {
    let s = input.trim().to_lowercase();
    let (digits, unit) = s.split_at(s.len().saturating_sub(1));
    let n: i64 = digits
        .parse()
        .map_err(|_| "snooze takes whole hours or days, like 3h or 2d".to_owned())?;
    let secs = match unit {
        "h" => n * HOUR,
        "d" => n * DAY,
        _ => return Err("snooze takes whole hours or days, like 3h or 2d".to_owned()),
    };
    if n < 1 || secs > MAX_SNOOZE_DAYS * DAY {
        return Err(format!("snooze must be 1 to {MAX_SNOOZE_DAYS} days"));
    }
    Ok(secs)
}

/// Start of the local day containing `now` (`off` is the local offset from UTC).
#[must_use]
pub fn day_start(now: i64, off: i64) -> i64 {
    let local = now + off;
    local - local.rem_euclid(DAY) - off
}

/// Tomorrow at 09:00 local.
#[must_use]
pub fn tomorrow_morning(now: i64, off: i64) -> i64 {
    day_start(now, off) + DAY + DEFAULT_REMINDER_HOUR * HOUR
}

/// The coming Monday at 09:00 local (a Monday itself gives the next one).
#[must_use]
pub fn next_week(now: i64, off: i64) -> i64 {
    // 1970-01-01 was a Thursday, so Monday is 3 days later: (days + 3) % 7 == 0.
    let weekday = ((now + off).div_euclid(DAY) + 3).rem_euclid(7); // Monday = 0
    day_start(now, off) + (7 - weekday) * DAY + DEFAULT_REMINDER_HOUR * HOUR
}

/// Three hours from now.
#[must_use]
pub fn later(now: i64) -> i64 {
    now + LATER_SECS
}

/// A due date from the custom prompt (local time). Empty means no date. Accepts `today`,
/// `tomorrow`, `+Nd`, `YYYY-MM-DD`, each optionally followed by `HH:MM`.
///
/// # Errors
/// A short message for anything else.
pub fn parse_due(input: &str, now: i64, off: i64) -> Result<Option<i64>, String> {
    let s = input.trim().to_lowercase();
    if s.is_empty() {
        return Ok(None);
    }
    let bad = || "date like 2026-11-20, tomorrow or +3d, optionally followed by 09:00".to_owned();
    let mut parts = s.split_whitespace();
    let day_part = parts.next().ok_or_else(bad)?;
    let time_part = parts.next();
    if parts.next().is_some() {
        return Err(bad());
    }
    let midnight = if day_part == "today" {
        day_start(now, off)
    } else if day_part == "tomorrow" {
        day_start(now, off) + DAY
    } else if let Some(n) = day_part.strip_prefix('+').and_then(|d| d.strip_suffix('d')) {
        day_start(now, off) + n.parse::<i64>().map_err(|_| bad())? * DAY
    } else {
        let mut it = day_part.split('-');
        let (y, m, d) = (it.next(), it.next(), it.next());
        let (Some(y), Some(m), Some(d), None) = (y, m, d, it.next()) else {
            return Err(bad());
        };
        let (y, m, d): (i64, i64, i64) = (
            y.parse().map_err(|_| bad())?,
            m.parse().map_err(|_| bad())?,
            d.parse().map_err(|_| bad())?,
        );
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return Err(bad());
        }
        from_civil(y, m, d, 0, 0) - off
    };
    let offset = match time_part {
        None => DEFAULT_REMINDER_HOUR * HOUR,
        Some(t) => {
            let (h, m) = t.split_once(':').ok_or_else(bad)?;
            let (h, m): (i64, i64) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
            if !(0..24).contains(&h) || !(0..60).contains(&m) {
                return Err(bad());
            }
            h * HOUR + m * 60
        }
    };
    Ok(Some(midnight + offset))
}

/// The suggested due time for a thread: a `YYYY-MM-DD` date in the subject (09:00 that day, if
/// still ahead), otherwise tomorrow morning.
#[must_use]
pub fn suggest_due(subject: &str, now: i64, off: i64) -> i64 {
    // `get` returns None where a window would cut a multi-byte character (a subject with an
    // accent in it), so no position can panic
    for i in 0..subject.len().saturating_sub(9) {
        let Some(c) = subject.get(i..i + 10) else {
            continue;
        };
        let ok = c.bytes().enumerate().all(|(k, x)| {
            if k == 4 || k == 7 {
                x == b'-'
            } else {
                x.is_ascii_digit()
            }
        });
        if ok {
            if let Ok(Some(t)) = parse_due(c, now, off) {
                if t > now {
                    return t;
                }
            }
        }
    }
    tomorrow_morning(now, off)
}

/// `YYYY-MM-DD HH:MM` in local time.
#[must_use]
pub fn stamp(secs: i64, off: i64) -> String {
    let (y, m, d, h, mi) = to_civil(secs + off);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// A message time that reads naturally while it is recent: `12 seconds ago`, `5 minutes ago`,
/// `3 hours ago`, then `Fri 12:30` up to a week back, then the full date and time. A time in
/// the future (a clock that disagrees) is shown as the full date and time.
#[must_use]
pub fn smart_stamp(secs: i64, now: i64, off: i64) -> String {
    const NAMES: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let age = now - secs;
    let plural = |n: i64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match age {
        a if a < 0 => stamp(secs, off),
        a if a < 60 => plural(a, "second"),
        a if a < 3_600 => plural(a / 60, "minute"),
        a if a < 86_400 => plural(a / 3_600, "hour"),
        a if a < 7 * 86_400 => {
            let local = secs + off;
            let (_, _, _, h, mi) = to_civil(local);
            let day = usize::try_from(local.div_euclid(86_400).rem_euclid(7)).unwrap_or(0);
            format!("{} {h:02}:{mi:02}", NAMES[day])
        }
        _ => stamp(secs, off),
    }
}

/// `Thu 12 Nov` for the local day containing `secs`.
#[must_use]
pub fn day_label(secs: i64, off: i64) -> String {
    const NAMES: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let local = secs + off;
    let (_, m, d, _, _) = to_civil(local);
    let weekday = NAMES[usize::try_from(local.div_euclid(DAY).rem_euclid(7)).unwrap_or(0)];
    format!(
        "{weekday} {d} {}",
        MONTHS[usize::try_from(m - 1).unwrap_or(0)]
    )
}

/// `Fri 13 Nov 09:00` (local), for the reminder prompt.
#[must_use]
pub fn describe(secs: i64, off: i64) -> String {
    format!("{} {}", day_label(secs, off), &stamp(secs, off)[11..])
}

/// First row to draw so that `selected` stays inside a window of `rows` rows without the list
/// jumping: it moves only when the cursor leaves the window.
#[must_use]
pub fn window_start(current: usize, selected: usize, rows: usize) -> usize {
    if rows == 0 || selected < current {
        selected
    } else if selected >= current + rows {
        selected + 1 - rows
    } else {
        current
    }
}

/// Text safe to draw: a message body comes from outside, so control characters (terminal escape
/// sequences) and bidirectional overrides are removed; tabs become spaces, line breaks stay.
#[must_use]
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            '\r' => None,
            c if c.is_control() => None,
            // left-to-right and right-to-left embeddings, overrides and isolates
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' => None,
            c => Some(c),
        })
        .collect()
}

/// Terminal columns `s` takes (wide letters count two, combining marks and zero-width ones none).
#[must_use]
pub fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Break `text` into lines of at most `width` columns, at spaces where possible. Blank lines
/// are kept; a word longer than a line is cut.
#[must_use]
pub fn wrap(text: &str, width_cols: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let limit = width_cols.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut cur_len = 0_usize;
        for word in line.split(' ') {
            let w_len = width(word);
            if cur_len > 0 && cur_len + 1 + w_len > limit {
                out.push(std::mem::take(&mut cur));
                cur_len = 0;
            }
            if w_len > limit {
                // cut a long word into full lines
                if cur_len > 0 {
                    out.push(std::mem::take(&mut cur));
                }
                cur_len = 0;
                for ch in word.chars() {
                    let w = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if cur_len + w > limit && cur_len > 0 {
                        out.push(std::mem::take(&mut cur));
                        cur_len = 0;
                    }
                    cur.push(ch);
                    cur_len += w;
                }
                continue;
            }
            if cur_len > 0 {
                cur.push(' ');
                cur_len += 1;
            }
            cur.push_str(word);
            cur_len += w_len;
        }
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use haste_core::model::{ObjectKind, Source, Thread, Triage};

    fn view(source: Source, subject: &str, who: &[&str], priority: i32) -> ThreadView {
        let mut triage = Triage::new("t", ObjectKind::Thread);
        triage.priority = priority;
        ThreadView {
            thread: Thread {
                id: "t".into(),
                source,
                external_id: "e".into(),
                subject: subject.into(),
                participants: who.iter().map(|s| (*s).to_owned()).collect(),
                last_activity_at: 0,
                version: 1,
            },
            triage,
            tags: vec![],
            last: None,
            unread: false,
            replied: false,
            sender_known: true,
            sender: None,
        }
    }

    #[test]
    fn filters_and_their_keys() {
        let t = view(Source::Gmail, "Invoice due", &["Boss@Corp.com"], 1);
        let f = |sender: &[&str], source: &[&str], min: Option<i32>| FilterView {
            name: "f".into(),
            sender: sender.iter().map(|s| (*s).to_owned()).collect(),
            source: source.iter().map(|s| (*s).to_owned()).collect(),
            min_priority: min,
            ..FilterView::default()
        };
        assert!(matches(&FilterView::default(), &t));
        assert!(matches(&f(&["boss@corp"], &["gmail"], Some(1)), &t));
        assert!(!matches(&f(&["other"], &[], None), &t));
        assert!(!matches(&f(&[], &["whatsapp"], None), &t));
        assert!(!matches(&f(&[], &[], Some(2)), &t));
    }

    #[test]
    fn snooze_is_whole_hours_or_days() {
        assert_eq!(parse_snooze("3h"), Ok(3 * 3600));
        assert_eq!(parse_snooze(" 2D "), Ok(2 * 86_400));
        for bad in ["1.5h", "0h", "h", "3", "3m", "999d", "", "-1h"] {
            assert!(parse_snooze(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn due_dates_are_local_time() {
        let off = 3600; // CET
        let now = from_civil(2026, 11, 12, 14, 0); // 15:00 local
        assert_eq!(parse_due("", now, off), Ok(None));
        // tomorrow 09:00 local is 08:00 UTC
        assert_eq!(
            parse_due("tomorrow", now, off),
            Ok(Some(from_civil(2026, 11, 13, 8, 0)))
        );
        assert_eq!(
            parse_due("today 17:30", now, off),
            Ok(Some(from_civil(2026, 11, 12, 16, 30)))
        );
        assert_eq!(
            parse_due("2026-12-01 08:15", now, off),
            Ok(Some(from_civil(2026, 12, 1, 7, 15)))
        );
        for bad in [
            "soon",
            "2026-13-01",
            "tomorrow 25:00",
            "tomorrow 9",
            "1 2 3",
        ] {
            assert!(parse_due(bad, now, off).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_quick_picks() {
        let off = 3600;
        // Thursday 12 Nov 2026, 23:30 local (22:30 UTC): "tomorrow" is Friday the 13th
        let now = from_civil(2026, 11, 12, 22, 30);
        assert_eq!(tomorrow_morning(now, off), from_civil(2026, 11, 13, 8, 0));
        // next week = Monday 16 Nov 09:00 local
        assert_eq!(next_week(now, off), from_civil(2026, 11, 16, 8, 0));
        // on a Monday it is the Monday after
        let monday = from_civil(2026, 11, 16, 10, 0);
        assert_eq!(next_week(monday, off), from_civil(2026, 11, 23, 8, 0));
        // on a Sunday it is the next day
        let sunday = from_civil(2026, 11, 15, 10, 0);
        assert_eq!(next_week(sunday, off), from_civil(2026, 11, 16, 8, 0));
        assert_eq!(later(now), now + 3 * 3600);
    }

    #[test]
    fn the_suggestion_comes_from_the_subject_or_is_tomorrow_morning() {
        let off = 0;
        let now = from_civil(2026, 11, 12, 15, 0);
        assert_eq!(suggest_due("Lunch", now, off), tomorrow_morning(now, off));
        assert_eq!(
            suggest_due("Report due 2026-11-20 please", now, off),
            from_civil(2026, 11, 20, 9, 0)
        );
        assert_eq!(
            suggest_due("Old 2026-01-01", now, off),
            tomorrow_morning(now, off),
            "a past date is not suggested"
        );
        assert_eq!(
            suggest_due("Odd 2026-99-99", now, off),
            tomorrow_morning(now, off)
        );
        // accents and other multi-byte characters anywhere in the subject must not panic
        for subject in [
            "Granát",
            "é",
            "Re: Köszönöm 2099-03-04 ✓",
            "日本語のメール",
            "✓✓✓✓✓✓✓✓✓✓✓",
        ] {
            let _ = suggest_due(subject, now, off);
        }
        assert_eq!(
            suggest_due("Találkozó 2099-03-04", now, off),
            from_civil(2099, 3, 4, 9, 0)
        );
    }

    #[test]
    fn the_cursor_window_moves_only_when_the_cursor_leaves_it() {
        assert_eq!(window_start(0, 3, 10), 0);
        assert_eq!(window_start(0, 12, 10), 3);
        assert_eq!(window_start(3, 5, 10), 3, "no jump while inside");
        assert_eq!(window_start(3, 2, 10), 2);
    }

    #[test]
    fn untrusted_text_loses_escape_sequences_and_bidi_overrides() {
        let nasty = "hi\u{1b}[2J\u{1b}]0;owned\u{7}\r\nthere\u{202E}evil\ttab";
        let clean = sanitize(nasty);
        assert!(!clean.contains('\u{1b}') && !clean.contains('\u{7}'));
        assert!(!clean.contains('\u{202E}') && !clean.contains('\r'));
        assert_eq!(clean, "hi[2J]0;owned\nthereevil tab");
        assert_eq!(sanitize("Granát é 日本"), "Granát é 日本");
    }

    #[test]
    fn wrapping_breaks_at_spaces_keeps_blank_lines_and_cuts_long_words() {
        assert_eq!(wrap("aaa bbb ccc", 7), vec!["aaa bbb", "ccc"]);
        assert_eq!(wrap("one\n\ntwo", 10), vec!["one", "", "two"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("é é é é", 3), vec!["é é", "é é"]);
        assert!(wrap("", 5) == vec![String::new()]);
    }

    #[test]
    fn short_senders() {
        assert_eq!(short_sender("Ada Lovelace <ada@x.org>"), "Ada Lovelace");
        assert_eq!(short_sender("ada@x.org"), "ada");
        assert_eq!(short_sender("+36301234567"), "+36301234567");
        assert_eq!(short_sender("noreply@accounts.google.com"), "google");
        assert_eq!(short_sender("No-Reply@shop.co.uk"), "shop");
        assert_eq!(short_sender("<donotreply@bank.hu>"), "bank");
        assert_eq!(short_sender("notifications@github.com"), "github");
        assert_eq!(short_sender("ana@x.org"), "ana");
        // a robot name with a unique tail is still a robot
        assert_eq!(
            short_sender("no-reply-qwrphp-krvoryje8moywcw@mail.claude.com"),
            "claude"
        );
        assert_eq!(short_sender("notifications+abc123@github.com"), "github");
        assert_eq!(short_sender("noreplyfan@x.org"), "noreplyfan");
        assert_eq!(
            short_sender_or_address("no-reply-ab-cd@mail.claude.com"),
            "claude"
        );
    }

    #[test]
    fn recent_times_read_naturally() {
        let now = from_civil(2026, 11, 15, 14, 0); // a Sunday
        assert_eq!(smart_stamp(now - 1, now, 0), "1 second ago");
        assert_eq!(smart_stamp(now - 45, now, 0), "45 seconds ago");
        assert_eq!(smart_stamp(now - 120, now, 0), "2 minutes ago");
        assert_eq!(smart_stamp(now - 3 * 3_600, now, 0), "3 hours ago");
        assert_eq!(
            smart_stamp(from_civil(2026, 11, 13, 12, 30), now, 0),
            "Fri 12:30"
        );
        assert_eq!(
            smart_stamp(from_civil(2026, 11, 1, 9, 5), now, 0),
            "2026-11-01 09:05"
        );
        assert_eq!(smart_stamp(now + 100, now, 0), stamp(now + 100, 0));
    }

    #[test]
    fn labels_are_local() {
        assert_eq!(stamp(0, 0), "1970-01-01 00:00");
        assert_eq!(stamp(0, 3600), "1970-01-01 01:00");
        assert_eq!(day_label(from_civil(2026, 11, 12, 0, 0), 0), "Thu 12 Nov");
        assert_eq!(
            describe(from_civil(2026, 11, 13, 8, 0), 3600),
            "Fri 13 Nov 09:00"
        );
    }
}
