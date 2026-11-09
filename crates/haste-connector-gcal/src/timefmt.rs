//! Minimal RFC 3339 / date handling (the connector needs no time crate).

use crate::consts::DAY_SECS;

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
#[must_use]
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[must_use]
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn num(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `YYYY-MM-DD` as unix seconds at 00:00 UTC.
#[must_use]
pub fn parse_date(s: &str) -> Option<i64> {
    let mut parts = s.split('-');
    let (y, m, d) = (
        num(parts.next()?)?,
        num(parts.next()?)?,
        num(parts.next()?)?,
    );
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * DAY_SECS)
}

/// `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)` as unix seconds.
#[must_use]
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once(['T', 't'])?;
    let day = parse_date(date)?;
    let (time, offset) = if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        (t, 0)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (t, off) = rest.split_at(at);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (oh, om) = off[1..].split_once(':')?;
        (t, sign * (num(oh)? * 3600 + num(om)? * 60))
    };
    let time = time.split('.').next()?;
    let mut hms = time.split(':');
    let (h, mi, sec) = (num(hms.next()?)?, num(hms.next()?)?, num(hms.next()?)?);
    if hms.next().is_some() || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(day + h * 3600 + mi * 60 + sec - offset)
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
#[must_use]
pub fn format_rfc3339(t: i64) -> String {
    let days = t.div_euclid(DAY_SECS);
    let secs = t.rem_euclid(DAY_SECS);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Unix seconds (a multiple of a day) as `YYYY-MM-DD`.
#[must_use]
pub fn format_date(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(DAY_SECS));
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-11-09T10:00:00Z"), Some(1_794_218_400));
        // an offset is subtracted, fractions are dropped
        assert_eq!(
            parse_rfc3339("2026-11-09T11:00:00+01:00"),
            Some(1_794_218_400)
        );
        assert_eq!(
            parse_rfc3339("2026-11-09T05:00:00.250-05:00"),
            Some(1_794_218_400)
        );
        assert_eq!(parse_date("2026-11-09"), Some(1_794_182_400));
    }

    #[test]
    fn formatting_round_trips_including_leap_days_and_pre_epoch() {
        for t in [0, 1_794_218_400, 951_782_400, 1_709_208_000, -86_400, -1] {
            assert_eq!(parse_rfc3339(&format_rfc3339(t)), Some(t), "{t}");
        }
        assert_eq!(format_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_date(1_794_182_400), "2026-11-09");
    }

    #[test]
    fn garbage_is_refused() {
        for bad in [
            "",
            "2026-13-01",
            "2026-11-09",
            "2026-11-09T25:00:00Z",
            "x-y-zT00:00:00Z",
            "2026-11-09T10:00:00",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }
}
