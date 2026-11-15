//! Dates and repeat rules written in plain words, English and Hungarian: `2 Nov`, `Tue`,
//! `tomorrow`, `kedd`, `next week`, `in 3 days`, `3d`, `2h`, `every week`, `minden hónap`.
//! Pure: a text goes in, a due time, a repeat rule and the words that were understood come out.
//! Whatever is not understood is ignored.

use haste_core::model::{RepeatFreq, RepeatRule};
use haste_core::time::{from_civil, to_civil};

const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;
/// Hour of day used when only a date is given.
const DEFAULT_HOUR: i64 = 9;

/// What a text says about when, and how often.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct When {
    /// Unix seconds.
    pub due: Option<i64>,
    pub repeat: Option<RepeatRule>,
    /// Byte ranges of the words that were understood, for showing them.
    pub marks: Vec<(usize, usize)>,
}

/// A word of the text: its plain form (lowercase, no accents) and where it stands.
struct Word {
    text: String,
    start: usize,
    end: usize,
}

fn plain(c: char) -> char {
    match c {
        'á' | 'à' | 'ä' => 'a',
        'é' | 'è' => 'e',
        'í' => 'i',
        'ó' | 'ö' | 'ő' => 'o',
        'ú' | 'ü' | 'ű' => 'u',
        c => c,
    }
}

/// Split into words at anything but letters, digits, `:`, `-` and `+`; each word lowercase and
/// without accents. Positions are byte offsets into `text`.
fn words(text: &str) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    let mut current: Option<(usize, String)> = None;
    for (at, c) in text.char_indices() {
        let c = c.to_lowercase().next().unwrap_or(c);
        if c.is_alphanumeric() || matches!(c, ':' | '-' | '+') {
            match &mut current {
                Some((_, w)) => w.push(plain(c)),
                None => current = Some((at, plain(c).to_string())),
            }
        } else if let Some((start, w)) = current.take() {
            out.push(Word {
                text: w,
                start,
                end: at,
            });
        }
    }
    if let Some((start, w)) = current {
        out.push(Word {
            text: w,
            start,
            end: text.len(),
        });
    }
    out
}

/// Monday = 0.
fn weekday_of(local_midnight: i64) -> i64 {
    (local_midnight.div_euclid(DAY) + 3).rem_euclid(7)
}

fn weekday_name(word: &str) -> Option<i64> {
    Some(match word {
        "mon" | "monday" | "hetfo" | "hetfon" => 0,
        "tue" | "tues" | "tuesday" | "kedd" | "kedden" => 1,
        "wed" | "wednesday" | "szerda" | "szerdan" => 2,
        "thu" | "thur" | "thurs" | "thursday" | "csutortok" | "csutortokon" => 3,
        "fri" | "friday" | "pentek" | "penteken" => 4,
        "sat" | "saturday" | "szombat" | "szombaton" => 5,
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

/// A day of the month: `2`, `2nd`, `3rd`, `21st`, `4th`.
fn day_number(word: &str) -> Option<i64> {
    let w = word.trim_end_matches('.');
    let digits = w
        .strip_suffix("st")
        .or_else(|| w.strip_suffix("nd"))
        .or_else(|| w.strip_suffix("rd"))
        .or_else(|| w.strip_suffix("th"))
        .unwrap_or(w);
    digits.parse::<i64>().ok().filter(|d| (1..=31).contains(d))
}

/// A lone number that is an hour: it follows a day word (`tue 10`, `tomorrow 9`) or `at`.
fn bare_hour(word: &str, previous: Option<&str>) -> Option<i64> {
    let h: i64 = word.parse().ok().filter(|h| (0..24).contains(h))?;
    let after_day = previous.is_some_and(|p| {
        matches!(
            p,
            "at" | "today" | "tomorrow" | "tmrw" | "tmr" | "ma" | "holnap" | "holnaputan" | "kor"
        ) || weekday_name(p).is_some()
    });
    after_day.then_some(h)
}

/// `3d`, `2h`, `1w`, `+3d`: a span from now.
fn short_span(word: &str) -> Option<(i64, char)> {
    let w = word.trim_start_matches('+');
    let unit = w.chars().last()?;
    if !matches!(unit, 'd' | 'h' | 'w') {
        return None;
    }
    let n: i64 = w[..w.len() - 1].parse().ok()?;
    (1..=999).contains(&n).then_some((n, unit))
}

/// `3 days`, `2 hours`, `4 nap`, `2 het`: the unit as a letter (`m` minute, `h`, `d`, `w`, `M`).
fn unit_word(word: &str) -> Option<char> {
    Some(match word {
        "minute" | "minutes" | "min" | "mins" | "perc" => 'm',
        "hour" | "hours" | "hr" | "hrs" | "ora" | "orat" => 'h',
        "day" | "days" | "nap" | "napot" => 'd',
        "week" | "weeks" | "het" | "hetet" => 'w',
        "month" | "months" | "honap" | "honapot" => 'M',
        _ => return None,
    })
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

/// Words that stand for a time of day.
fn time_of_day(word: &str) -> Option<i64> {
    Some(match word {
        "morning" | "reggel" => 9 * 60,
        "noon" | "midday" | "delben" => 12 * 60,
        "afternoon" | "delutan" => 15 * 60,
        "evening" | "tonight" | "este" => 18 * 60,
        _ => return None,
    })
}

fn repeat_unit(word: &str) -> Option<RepeatFreq> {
    Some(match word {
        "day" | "days" | "daily" | "everyday" | "nap" | "naponta" => RepeatFreq::Day,
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
    let ws = words(text);
    let today = (now + off).div_euclid(DAY) * DAY; // local midnight, as a "local clock" second
    let mut day: Option<i64> = None; // local midnight of the day found
    let mut minutes: Option<i64> = None;
    let mut relative: Option<i64> = None; // an exact moment from now
    let mut repeat: Option<RepeatRule> = None;
    let mut used: Vec<usize> = Vec::new();

    let mut i = 0;
    while i < ws.len() {
        let w = ws[i].text.as_str();
        let next = ws.get(i + 1).map(|x| x.text.as_str());
        let after = ws.get(i + 2).map(|x| x.text.as_str());
        // repeating: "every week", "every 2 weeks", "every other week", "every tue", "weekly"
        if matches!(w, "every" | "each" | "minden") {
            let mut set = |freq, every, took: usize, used: &mut Vec<usize>| {
                repeat = Some(RepeatRule { freq, every });
                used.extend(i..i + took);
            };
            if let (Some(n), Some(freq)) = (
                next.and_then(|n| n.parse::<u32>().ok()),
                after.and_then(repeat_unit),
            ) {
                set(freq, n.clamp(1, 999), 3, &mut used);
                i += 3;
                continue;
            }
            if next == Some("other") {
                if let Some(freq) = after.and_then(repeat_unit) {
                    set(freq, 2, 3, &mut used);
                    i += 3;
                    continue;
                }
            }
            if let Some(freq) = next.and_then(repeat_unit) {
                set(freq, 1, 2, &mut used);
                i += 2;
                continue;
            }
            if let Some(wd) = next.and_then(weekday_name) {
                set(RepeatFreq::Week, 1, 2, &mut used);
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
                | "everyday"
                | "naponta"
                | "hetente"
                | "havonta"
                | "evente"
        ) {
            if let Some(freq) = repeat_unit(w) {
                repeat = Some(RepeatRule { freq, every: 1 });
                used.push(i);
            }
            i += 1;
            continue;
        }
        // "3d", "2h" / "in 3 days", "2 hours", "3 nap mulva"
        let counted = short_span(w).or_else(|| {
            let n: i64 = w.trim_start_matches('+').parse().ok()?;
            let unit = next.and_then(unit_word)?;
            (1..=999).contains(&n).then_some((n, unit))
        });
        if let Some((n, unit)) = counted {
            let words_used = if short_span(w).is_some() { 1 } else { 2 };
            match unit {
                'm' => relative = Some(now + n * 60),
                'h' => relative = Some(now + n * HOUR),
                'd' => day = Some(today + n * DAY),
                'w' => day = Some(today + 7 * n * DAY),
                _ => {
                    day = Some(
                        crate::inbox::step_repeat(
                            today - off,
                            RepeatRule {
                                freq: RepeatFreq::Month,
                                every: 1,
                            },
                            n,
                            off,
                        ) + off,
                    );
                }
            }
            used.extend(i..i + words_used);
            i += words_used;
            // "múlva" / "from now" after it
            if matches!(ws.get(i).map(|x| x.text.as_str()), Some("mulva")) {
                used.push(i);
                i += 1;
            }
            continue;
        }
        let mut took = 1;
        if matches!(w, "today" | "ma") {
            day = Some(today);
        } else if matches!(w, "tomorrow" | "tmrw" | "tmr" | "holnap") {
            day = Some(today + DAY);
        } else if w == "holnaputan" {
            day = Some(today + 2 * DAY);
        } else if (w == "next" && next == Some("week")) || (w == "jovo" && next == Some("het")) {
            let ahead = 7 - weekday_of(today);
            day = Some(today + ahead * DAY);
            took = 2;
        } else if (w == "next" && next == Some("month")) || (w == "jovo" && next == Some("honap")) {
            let rule = RepeatRule {
                freq: RepeatFreq::Month,
                every: 1,
            };
            day = Some(crate::inbox::step_repeat(today - off, rule, 1, off) + off);
            took = 2;
        } else if let Some(wd) = weekday_name(w) {
            let ahead = (wd - weekday_of(today)).rem_euclid(7);
            day = Some(today + (if ahead == 0 { 7 } else { ahead }) * DAY);
        } else if let Some(m) = clock(w) {
            minutes = Some(m);
        } else if let Some(m) = time_of_day(w) {
            minutes = minutes.or(Some(m));
            if w == "tonight" && day.is_none() {
                day = Some(today);
            }
        } else if w.len() == 10 && w.as_bytes().get(4) == Some(&b'-') {
            // 2026-11-20
            let mut it = w.split('-');
            let (y, m, d) = (
                it.next().and_then(|v| v.parse::<i64>().ok()),
                it.next().and_then(|v| v.parse::<i64>().ok()),
                it.next().and_then(|v| v.parse::<i64>().ok()),
            );
            match (y, m, d) {
                (Some(y), Some(m), Some(d)) if (1..=12).contains(&m) && (1..=31).contains(&d) => {
                    day = Some(from_civil(y, m, d, 0, 0));
                }
                _ => took = 0,
            }
        } else {
            // "2 nov" / "nov 2" / "2nd of november" (a month name beside a day number)
            let of = next == Some("of");
            let month_at = if of { after } else { next };
            let pair = match (
                day_number(w),
                month_at.and_then(month_name),
                month_name(w),
                next.and_then(day_number),
            ) {
                (Some(d), Some(m), _, _) => {
                    took = if of { 3 } else { 2 };
                    Some((m, d))
                }
                (_, _, Some(m), Some(d)) => {
                    took = 2;
                    Some((m, d))
                }
                _ => None,
            };
            if let Some((m, d)) = pair {
                let (y, ..) = to_civil(today);
                let mut at = from_civil(y, m, d, 0, 0);
                if at < today {
                    at = from_civil(y + 1, m, d, 0, 0);
                }
                day = Some(at);
            } else if let Some(h) = bare_hour(w, i.checked_sub(1).map(|k| ws[k].text.as_str())) {
                // "Tue 10", "tomorrow 9", "at 5": a lone number after a day word is an hour
                minutes = Some(h * 60);
            } else {
                took = 0;
            }
        }
        used.extend(i..i + took);
        i += took.max(1);
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
    let mut marks: Vec<(usize, usize)> = Vec::new();
    used.sort_unstable();
    used.dedup();
    for k in used {
        let (s, e) = (ws[k].start, ws[k].end);
        match marks.last_mut() {
            Some(last) if text[last.1..s].trim().is_empty() => last.1 = e,
            _ => marks.push((s, e)),
        }
    }
    When { due, repeat, marks }
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
    fn counted_spans_ordinals_and_other_phrasings_are_understood() {
        assert_eq!(due("in 3 days"), Some(at(2026, 11, 18, 9, 0)));
        assert_eq!(due("in 2 hours"), Some(NOW + 2 * HOUR));
        assert_eq!(due("30 minutes"), Some(NOW + 30 * 60));
        assert_eq!(due("2 weeks"), Some(at(2026, 11, 29, 9, 0)));
        assert_eq!(due("3 nap múlva"), Some(at(2026, 11, 18, 9, 0)));
        assert_eq!(due("2 hét múlva"), Some(at(2026, 11, 29, 9, 0)));
        assert_eq!(due("2nd nov"), Some(at(2027, 11, 2, 9, 0)));
        assert_eq!(due("nov 21st"), Some(at(2026, 11, 21, 9, 0)));
        assert_eq!(due("the 2nd of december"), Some(at(2026, 12, 2, 9, 0)));
        assert_eq!(due("next month"), Some(at(2026, 12, 15, 9, 0)));
        assert_eq!(due("tmrw"), Some(at(2026, 11, 16, 9, 0)));
        assert_eq!(due("tonight"), Some(at(2026, 11, 15, 18, 0)));
        assert_eq!(due("friday morning"), Some(at(2026, 11, 20, 9, 0)));
        assert_eq!(due("tomorrow evening"), Some(at(2026, 11, 16, 18, 0)));
        assert_eq!(due("hétfőn"), Some(at(2026, 11, 16, 9, 0)));
        assert_eq!(due("pénteken 10:00"), Some(at(2026, 11, 20, 10, 0)));
        let rule = |t: &str| parse(t, NOW, OFF).repeat;
        assert_eq!(
            rule("every other week"),
            Some(RepeatRule {
                freq: RepeatFreq::Week,
                every: 2
            })
        );
        assert_eq!(
            rule("everyday"),
            Some(RepeatRule {
                freq: RepeatFreq::Day,
                every: 1
            })
        );
    }

    #[test]
    fn the_understood_words_are_marked_for_showing() {
        let text = "Call Ana tomorrow at 14:30 every week please";
        let when = parse(text, NOW, OFF);
        let shown: Vec<&str> = when.marks.iter().map(|(s, e)| &text[*s..*e]).collect();
        // neighbouring understood words form one run
        assert_eq!(shown, ["tomorrow", "14:30 every week"]);
        // accents and capitals keep their own positions
        let text = "Jövő Kedd 10:00";
        let shown: Vec<&str> = parse(text, NOW, OFF)
            .marks
            .iter()
            .map(|(s, e)| &text[*s..*e])
            .collect();
        assert_eq!(shown, ["Kedd 10:00"]);
        assert!(parse("Quarterly plan", NOW, OFF).marks.is_empty());
    }

    #[test]
    fn a_lone_number_after_a_day_word_is_an_hour() {
        // 2026-11-15 is a Sunday: the next Tuesday is the 17th
        assert_eq!(due("Tue 10"), Some(at(2026, 11, 17, 10, 0)));
        assert_eq!(due("next Tuesday 10"), Some(at(2026, 11, 17, 10, 0)));
        assert_eq!(due("kedd 8"), Some(at(2026, 11, 17, 8, 0)));
        assert_eq!(due("tomorrow 9"), Some(at(2026, 11, 16, 9, 0)));
        assert_eq!(due("meet at 17"), Some(at(2026, 11, 15, 17, 0)));
        // a day and a month still win over an hour
        assert_eq!(due("Tue 10 nov"), Some(at(2027, 11, 10, 9, 0)));
        // no day word: a number is just a number
        assert_eq!(due("room 10"), None);
    }

    #[test]
    fn text_without_a_date_has_none() {
        assert_eq!(parse("Quarterly plan", NOW, OFF), When::default());
        assert_eq!(parse("", NOW, OFF), When::default());
        // the word "ma" is today in Hungarian only when it stands alone
        assert!(due("macska").is_none());
    }
}
