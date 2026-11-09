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
