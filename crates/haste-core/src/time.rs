//! Wall-clock helper kept in one place so tests can avoid it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The machine's current offset from UTC in seconds (`+0100` is 3600), read from `date +%z`;
/// 0 when it cannot be read. Changes at a daylight-saving switch, so callers refresh it now
/// and then.
#[must_use]
pub fn local_offset_secs() -> i64 {
    let out = std::process::Command::new("date").arg("+%z").output();
    let text = out
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    parse_utc_offset(&text).unwrap_or(0)
}

/// `+0100` or `-0530` as seconds.
#[must_use]
pub fn parse_utc_offset(s: &str) -> Option<i64> {
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    if rest.len() != 4 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (h, m): (i64, i64) = (rest[..2].parse().ok()?, rest[2..].parse().ok()?);
    (h < 24 && m < 60).then_some(sign * (h * 3600 + m * 60))
}

/// Unix seconds for a UTC calendar time.
#[must_use]
pub fn from_civil(y: i64, m: i64, d: i64, h: i64, mi: i64) -> i64 {
    days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60
}

/// `(year, month, day, hour, minute)` in UTC for unix seconds.
#[must_use]
pub fn to_civil(secs: i64) -> (i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, rem / 3600, rem % 3600 / 60)
}

fn digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)` as unix seconds, or `None` for anything else.
#[must_use]
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once(['T', 't'])?;
    let mut d = date.split('-');
    let (y, m, day) = (digits(d.next()?)?, digits(d.next()?)?, digits(d.next()?)?);
    if d.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let (time, offset) = if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        (t, 0)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (t, off) = rest.split_at(at);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (oh, om) = off[1..].split_once(':')?;
        (t, sign * (digits(oh)? * 3600 + digits(om)? * 60))
    };
    let mut hms = time.split('.').next()?.split(':');
    let (h, mi, sec) = (
        digits(hms.next()?)?,
        digits(hms.next()?)?,
        digits(hms.next()?)?,
    );
    if hms.next().is_some() || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(days_from_civil(y, m, day) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_offsets_parse() {
        assert_eq!(super::parse_utc_offset("+0100"), Some(3600));
        assert_eq!(super::parse_utc_offset("-0530"), Some(-19_800));
        for bad in ["", "0100", "+01", "+0160", "+2500", "CET"] {
            assert_eq!(super::parse_utc_offset(bad), None, "{bad}");
        }
    }

    #[test]
    fn civil_round_trips() {
        let t = super::from_civil(2026, 11, 12, 9, 30);
        assert_eq!(super::to_civil(t), (2026, 11, 12, 9, 30));
    }

    use super::*;

    #[test]
    fn rfc3339_instants_offsets_and_garbage() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-11-09T10:00:00Z"), Some(1_794_218_400));
        assert_eq!(
            parse_rfc3339("2026-11-09T11:00:00+01:00"),
            Some(1_794_218_400)
        );
        assert_eq!(
            parse_rfc3339("2026-11-09T05:00:00.250-05:00"),
            Some(1_794_218_400)
        );
        for bad in [
            "",
            "2026-13-01T00:00:00Z",
            "2026-11-09",
            "2026-11-09T25:00:00Z",
            "x",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }
}
