//! Pure helpers for the inbox tabs: filter matching, due-date and snooze prompts, day windows.

use haste_core::time::{from_civil, to_civil};
use haste_proto::{FilterView, ThreadView};

/// Channels shown on the Channels tab, in order.
pub const CHANNELS: [&str; 5] = ["gmail", "icloud", "whatsapp", "messenger", "imessage"];
/// How far ahead the Calendar tab looks.
pub const AGENDA_DAYS: i64 = 7;
/// Longest snooze accepted.
pub const MAX_SNOOZE_DAYS: i64 = 365;
/// Hour of day (UTC) used for a reminder when only a date is given.
pub const DEFAULT_REMINDER_HOUR: i64 = 9;
const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;

/// Mail channels, where a reply can address several people.
#[must_use]
pub fn is_mail(source: &str) -> bool {
    matches!(source, "gmail" | "icloud")
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

/// Start of the UTC day containing `now`.
#[must_use]
pub fn day_start(now: i64) -> i64 {
    now - now.rem_euclid(DAY)
}

/// A due date from the reminder prompt (UTC). Empty means no date. Accepts `today`, `tomorrow`,
/// `+Nd`, `YYYY-MM-DD`, each optionally followed by `HH:MM`.
///
/// # Errors
/// A short message for anything else.
pub fn parse_due(input: &str, now: i64) -> Result<Option<i64>, String> {
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
        day_start(now)
    } else if day_part == "tomorrow" {
        day_start(now) + DAY
    } else if let Some(n) = day_part.strip_prefix('+').and_then(|d| d.strip_suffix('d')) {
        day_start(now) + n.parse::<i64>().map_err(|_| bad())? * DAY
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
        from_civil(y, m, d, 0, 0)
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

/// The prompt's starting text: a `YYYY-MM-DD` date found in the subject, else tomorrow morning.
#[must_use]
pub fn default_due(subject: &str) -> String {
    let b = subject.as_bytes();
    for i in 0..b.len().saturating_sub(9) {
        let w = &subject[i..];
        if w.len() >= 10 && w.is_char_boundary(10) {
            let c = &w[..10];
            let ok = c.bytes().enumerate().all(|(k, x)| {
                if k == 4 || k == 7 {
                    x == b'-'
                } else {
                    x.is_ascii_digit()
                }
            });
            if ok && parse_due(c, 0).is_ok() {
                return format!("{c} {DEFAULT_REMINDER_HOUR:02}:00");
            }
        }
    }
    format!("tomorrow {DEFAULT_REMINDER_HOUR:02}:00")
}

/// `YYYY-MM-DD HH:MM` (UTC).
#[must_use]
pub fn stamp(secs: i64) -> String {
    let (y, m, d, h, mi) = to_civil(secs);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// `Thu 12 Nov` for day headers (UTC).
#[must_use]
pub fn day_label(secs: i64) -> String {
    const NAMES: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (_, m, d, _, _) = to_civil(secs);
    let weekday = NAMES[usize::try_from(secs.div_euclid(DAY).rem_euclid(7)).unwrap_or(0)];
    format!(
        "{weekday} {d} {}",
        MONTHS[usize::try_from(m - 1).unwrap_or(0)]
    )
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
    fn due_dates() {
        let now = from_civil(2026, 11, 12, 15, 0);
        assert_eq!(parse_due("", now), Ok(None));
        assert_eq!(
            parse_due("tomorrow", now),
            Ok(Some(from_civil(2026, 11, 13, 9, 0)))
        );
        assert_eq!(
            parse_due("today 17:30", now),
            Ok(Some(from_civil(2026, 11, 12, 17, 30)))
        );
        assert_eq!(
            parse_due("+3d", now),
            Ok(Some(from_civil(2026, 11, 15, 9, 0)))
        );
        assert_eq!(
            parse_due("2026-12-01 08:15", now),
            Ok(Some(from_civil(2026, 12, 1, 8, 15)))
        );
        for bad in [
            "soon",
            "2026-13-01",
            "tomorrow 25:00",
            "tomorrow 9",
            "1 2 3",
        ] {
            assert!(parse_due(bad, now).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_default_due_comes_from_the_subject_or_is_tomorrow() {
        assert_eq!(default_due("Lunch"), "tomorrow 09:00");
        assert_eq!(
            default_due("Report due 2026-11-20 please"),
            "2026-11-20 09:00"
        );
        assert_eq!(default_due("Odd 2026-99-99"), "tomorrow 09:00");
    }

    #[test]
    fn labels() {
        assert_eq!(stamp(0), "1970-01-01 00:00");
        assert_eq!(day_label(from_civil(2026, 11, 12, 0, 0)), "Thu 12 Nov");
    }
}
