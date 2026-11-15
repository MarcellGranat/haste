//! Dates and repeat rules written in plain words, English and Hungarian: `2 Nov`, `Tue`,
//! `tomorrow`, `kedd`, `next week`, `3d`, `2h`, `every week`, `minden hónap`. Pure: a text goes
//! in, a due time and a repeat rule come out. Whatever is not understood is ignored.

use haste_core::model::{RepeatFreq, RepeatRule};
use haste_core::time::{from_civil, to_civil};

const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;
/// Hour of day used when only a date is given.
const DEFAULT_HOUR: i64 = 9;

/// What a text says about when, and how often.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct When {
    /// Unix seconds.
    pub due: Option<i64>,
    pub repeat: Option<RepeatRule>,
}

/// Lowercase, accents taken off (Hungarian included), punctuation turned into spaces except
/// `:` and `-` and `+`.
fn normalise(text: &str) -> Vec<String> {
    let plain: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' => 'a',
            'é' | 'è' => 'e',
            'í' => 'i',
            'ó' | 'ö' | 'ő' => 'o',
            'ú' | 'ü' | 'ű' => 'u',
            c if c.is_alphanumeric() || matches!(c, ':' | '-' | '+') => c,
            _ => ' ',
        })
        .collect();
    plain.split_whitespace().map(str::to_owned).collect()
}

/// Monday = 0.
fn weekday_of(local_midnight_utc_equiv: i64) -> i64 {
    (local_midnight_utc_equiv.div_euclid(DAY) + 3).rem_euclid(7)
}

fn weekday_name(word: &str) -> Option<i64> {
    Some(match word {
        "mon" | "monday" | "hetfo" => 0,
        "tue" | "tues" | "tuesday" | "kedd" => 1,
        "wed" | "wednesday" | "szerda" => 2,
        "thu" | "thur" | "thurs" | "thursday" | "csutortok" => 3,
        "fri" | "friday" | "pentek" => 4,
        "sat" | "saturday" | "szombat" => 5,
        "sun" | "sunday" | "vasarnap" => 6,
        _ => return None,
    })
}

fn month_name(word: &str) -> Option<i64> {
    // the first three letters tell the month apart in English and Hungarian
    let w = word.trim_end_matches('.');
    if w.len() < 3 || !w.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(match &w[..3] {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" | "maj" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" | "sze" if w.starts_with("sep") || w.starts_with("szept") => 9,
        "oct" | "okt" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    })
}

/// `3d`, `2h`, `1w`, `+3d`: a span from now.
fn span(word: &str) -> Option<(i64, char)> {
    let w = word.trim_start_matches('+');
    let unit = w.chars().last()?;
    if !matches!(unit, 'd' | 'h' | 'w') {
        return None;
    }
    let n: i64 = w[..w.len() - 1].parse().ok()?;
    (1..=999).contains(&n).then_some((n, unit))
}

/// `14:30` or `9am` / `5pm`: minutes after midnight.
fn clock(word: &str) -> Option<i64> {
    if let Some((h, m)) = word.split_once(':') {
        let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
        return ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 60 + m);
    }
    let (digits, pm) = match (word.strip_suffix("am"), word.strip_suffix("pm")) {
        (Some(d), _) => (d, false),
        (_, Some(d)) => (d, true),
        _ => return None,
    };
    let h: i64 = digits.parse().ok()?;
    if !(1..=12).contains(&h) {
        return None;
    }
    Some(((h % 12) + if pm { 12 } else { 0 }) * 60)
}

fn repeat_unit(word: &str) -> Option<RepeatFreq> {
    Some(match word {
        "day" | "days" | "daily" | "nap" | "naponta" => RepeatFreq::Day,
        "week" | "weeks" | "weekly" | "het" | "hete" | "hetente" => RepeatFreq::Week,
        "month" | "months" | "monthly" | "honap" | "havonta" => RepeatFreq::Month,
        "year" | "years" | "yearly" | "annually" | "ev" | "evente" => RepeatFreq::Year,
        _ => return None,
    })
}

/// Read a due time and a repeat rule out of `text`. `now` is unix seconds and `off` the local
/// offset from UTC in seconds.
#[must_use]
pub fn parse(text: &str, now: i64, off: i64) -> When {
    let words = normalise(text);
    let today = (now + off).div_euclid(DAY) * DAY; // local midnight, as a "local clock" second
    let mut day: Option<i64> = None; // local midnight of the day found
    let mut minutes: Option<i64> = None;
    let mut relative: Option<i64> = None; // an exact moment from now
    let mut repeat: Option<RepeatRule> = None;

    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        let next = words.get(i + 1).map(String::as_str);
        // repeating: "every week", "every 2 weeks", "every tue", "weekly", "minden honap"
        if matches!(w, "every" | "each" | "minden") {
            if let Some(n) = next.and_then(|n| n.parse::<u32>().ok()) {
                if let Some(freq) = words.get(i + 2).and_then(|u| repeat_unit(u)) {
                    repeat = Some(RepeatRule {
                        freq,
                        every: n.clamp(1, 999),
                    });
                    i += 3;
                    continue;
                }
            }
            if let Some(freq) = next.and_then(repeat_unit) {
                repeat = Some(RepeatRule { freq, every: 1 });
                i += 2;
                continue;
            }
            if let Some(wd) = next.and_then(weekday_name) {
                repeat = Some(RepeatRule {
                    freq: RepeatFreq::Week,
                    every: 1,
                });
                let ahead = (wd - weekday_of(today)).rem_euclid(7);
                day = Some(today + (if ahead == 0 { 7 } else { ahead }) * DAY);
                i += 2;
                continue;
            }
        }
        if matches!(
            w,
            "daily"
                | "weekly"
                | "monthly"
                | "yearly"
                | "annually"
                | "naponta"
                | "hetente"
                | "havonta"
                | "evente"
        ) {
            if let Some(freq) = repeat_unit(w) {
                repeat = Some(RepeatRule { freq, every: 1 });
            }
            i += 1;
            continue;
        }
        if let Some((n, unit)) = span(w) {
            match unit {
                'h' => relative = Some(now + n * HOUR),
                'd' => day = Some(today + n * DAY),
                _ => day = Some(today + 7 * n * DAY),
            }
        } else if matches!(w, "today" | "ma") {
            day = Some(today);
        } else if matches!(w, "tomorrow" | "holnap") {
            day = Some(today + DAY);
        } else if w == "holnaputan" {
            day = Some(today + 2 * DAY);
        } else if (w == "next" && matches!(next, Some("week")))
            || (w == "jovo" && matches!(next, Some("het")))
        {
            let ahead = 7 - weekday_of(today);
            day = Some(today + ahead * DAY);
            i += 1;
        } else if let Some(wd) = weekday_name(w) {
            let ahead = (wd - weekday_of(today)).rem_euclid(7);
            day = Some(today + (if ahead == 0 { 7 } else { ahead }) * DAY);
        } else if let Some(m) = clock(w) {
            minutes = Some(m);
        } else if w.len() == 10 && w.as_bytes().get(4) == Some(&b'-') {
            // 2026-11-20
            let mut it = w.split('-');
            if let (Some(y), Some(m), Some(d)) = (
                it.next().and_then(|v| v.parse::<i64>().ok()),
                it.next().and_then(|v| v.parse::<i64>().ok()),
                it.next().and_then(|v| v.parse::<i64>().ok()),
            ) {
                if (1..=12).contains(&m) && (1..=31).contains(&d) {
                    day = Some(from_civil(y, m, d, 0, 0));
                }
            }
        } else {
            // "2 nov" / "nov 2" (a month name beside a day number)
            let as_day = |s: &str| {
                s.trim_end_matches('.')
                    .parse::<i64>()
                    .ok()
                    .filter(|d| (1..=31).contains(d))
            };
            let pair = match (
                as_day(w),
                next.and_then(month_name),
                month_name(w),
                next.and_then(as_day),
            ) {
                (Some(d), Some(m), _, _) => Some((m, d)),
                (_, _, Some(m), Some(d)) => Some((m, d)),
                _ => None,
            };
            if let Some((m, d)) = pair {
                let (y, ..) = to_civil(today);
                let mut at = from_civil(y, m, d, 0, 0);
                if at < today {
                    at = from_civil(y + 1, m, d, 0, 0);
                }
                day = Some(at);
                i += 1;
            }
        }
        i += 1;
    }

    let due = if let Some(at) = relative {
        Some(at - at.rem_euclid(60))
    } else if let Some(d) = day {
        Some(d + minutes.unwrap_or(DEFAULT_HOUR * 60) * 60 - off)
    } else if let Some(m) = minutes {
        // a time alone: today if still ahead, otherwise tomorrow
        let at = today + m * 60 - off;
        Some(if at > now { at } else { at + DAY })
    } else {
        None
    };
    When { due, repeat }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-15 (a Sunday) 12:00 UTC, with a local offset of +1 hour.
    const NOW: i64 = 1_794_744_000;
    const OFF: i64 = 3_600;

    fn at(y: i64, m: i64, d: i64, h: i64, mi: i64) -> i64 {
        from_civil(y, m, d, h, mi) - OFF
    }

    fn due(text: &str) -> Option<i64> {
        parse(text, NOW, OFF).due
    }

    #[test]
    fn the_day_of_the_test_is_what_it_says() {
        assert_eq!(to_civil(NOW + OFF).0, 2026);
        assert_eq!(weekday_of((NOW + OFF).div_euclid(DAY) * DAY), 6, "a Sunday");
    }

    #[test]
    fn plain_words_name_a_day() {
        assert_eq!(due("call back tomorrow"), Some(at(2026, 11, 16, 9, 0)));
        assert_eq!(due("holnap"), Some(at(2026, 11, 16, 9, 0)));
        assert_eq!(due("today"), Some(at(2026, 11, 15, 9, 0)));
        assert_eq!(
            due("next week"),
            Some(at(2026, 11, 16, 9, 0)),
            "the coming Monday"
        );
        assert_eq!(due("jövő hét"), Some(at(2026, 11, 16, 9, 0)));
    }

    #[test]
    fn weekdays_are_the_next_one_in_both_languages() {
        assert_eq!(due("Tue"), Some(at(2026, 11, 17, 9, 0)));
        assert_eq!(due("kedd"), Some(at(2026, 11, 17, 9, 0)));
        assert_eq!(due("csütörtök 14:30"), Some(at(2026, 11, 19, 14, 30)));
        // today is a Sunday: "sunday" means the next one
        assert_eq!(due("sunday"), Some(at(2026, 11, 22, 9, 0)));
    }

    #[test]
    fn day_and_month_work_in_either_order() {
        assert_eq!(
            due("2 Nov"),
            Some(at(2027, 11, 2, 9, 0)),
            "already past this year"
        );
        assert_eq!(due("Dec 24"), Some(at(2026, 12, 24, 9, 0)));
        assert_eq!(due("dec. 24."), Some(at(2026, 12, 24, 9, 0)));
        assert_eq!(due("3 szept"), Some(at(2027, 9, 3, 9, 0)));
        assert_eq!(due("on 2026-11-20"), Some(at(2026, 11, 20, 9, 0)));
    }

    #[test]
    fn spans_count_from_now() {
        assert_eq!(due("3d"), Some(at(2026, 11, 18, 9, 0)));
        assert_eq!(due("1w"), Some(at(2026, 11, 22, 9, 0)));
        assert_eq!(due("2h"), Some(NOW + 2 * HOUR));
    }

    #[test]
    fn a_time_alone_is_today_or_tomorrow() {
        assert_eq!(due("at 18:00"), Some(at(2026, 11, 15, 18, 0)));
        assert_eq!(
            due("at 9am"),
            Some(at(2026, 11, 16, 9, 0)),
            "9:00 has passed"
        );
        assert_eq!(due("tomorrow 5pm"), Some(at(2026, 11, 16, 17, 0)));
    }

    #[test]
    fn repeating_is_read_from_the_words() {
        let r = |t: &str| parse(t, NOW, OFF).repeat;
        let rule = |freq, every| Some(RepeatRule { freq, every });
        assert_eq!(r("pay rent every month"), rule(RepeatFreq::Month, 1));
        assert_eq!(r("every 2 weeks"), rule(RepeatFreq::Week, 2));
        assert_eq!(r("minden hónap"), rule(RepeatFreq::Month, 1));
        assert_eq!(r("naponta"), rule(RepeatFreq::Day, 1));
        assert_eq!(r("weekly report"), rule(RepeatFreq::Week, 1));
        assert_eq!(r("every year"), rule(RepeatFreq::Year, 1));
        assert_eq!(r("nothing here"), None);
        // "every Tuesday" is weekly and starts on the next Tuesday
        let w = parse("standup every tue", NOW, OFF);
        assert_eq!(w.repeat, rule(RepeatFreq::Week, 1));
        assert_eq!(w.due, Some(at(2026, 11, 17, 9, 0)));
    }

    #[test]
    fn text_without_a_date_has_none() {
        assert_eq!(parse("Quarterly plan", NOW, OFF), When::default());
        assert_eq!(parse("", NOW, OFF), When::default());
        // the word "ma" is today in Hungarian only when it stands alone
        assert!(due("macska").is_none());
    }
}
