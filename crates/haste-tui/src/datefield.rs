//! Typing a number into one part of a date: the day of the month, the hour or the minute. The
//! rest of the date stays as it is. Pure helpers for the date fields of the popups.

use haste_core::time::{from_civil, to_civil};

/// Which part of a date a typed number goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Day,
    Hour,
    Minute,
}

impl Part {
    /// Most digits that make sense for the part.
    #[must_use]
    pub fn max_digits(self) -> usize {
        2
    }
}

/// Days in a month of the given year.
fn month_len(y: i64, m: i64) -> i64 {
    (28..=31)
        .rev()
        .find(|d| to_civil(from_civil(y, m, *d, 0, 0)).1 == m)
        .unwrap_or(28)
}

/// `t` (unix seconds) with one part of its local date set to `value`, cut to what the part
/// allows (the day to the month's length, the hour to 23, the minute to 59).
#[must_use]
pub fn set_part(t: i64, off: i64, part: Part, value: i64) -> i64 {
    let (y, m, d, h, mi) = to_civil(t + off);
    let local = match part {
        Part::Day => from_civil(y, m, value.clamp(1, month_len(y, m)), h, mi),
        Part::Hour => from_civil(y, m, d, value.clamp(0, 23), mi),
        Part::Minute => from_civil(y, m, d, h, value.clamp(0, 59)),
    };
    local - off
}

/// Add a typed digit to what has been typed for a part. A third digit starts over, and so does a
/// number that is already too big for the part, so `1` then `4` is 14 and `4` then `5` for an
/// hour is 5 (45 is no hour).
#[must_use]
pub fn push_digit(typed: &str, digit: char, part: Part) -> String {
    let mut next = typed.to_owned();
    next.push(digit);
    let value: i64 = next.parse().unwrap_or(0);
    let limit = match part {
        Part::Day => 31,
        Part::Hour => 23,
        Part::Minute => 59,
    };
    if next.len() > part.max_digits() || value > limit {
        digit.to_string()
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFF: i64 = 3_600;

    fn at(y: i64, m: i64, d: i64, h: i64, mi: i64) -> i64 {
        from_civil(y, m, d, h, mi) - OFF
    }

    #[test]
    fn one_part_of_the_date_is_set_and_the_rest_stays() {
        let t = at(2026, 11, 18, 10, 30);
        assert_eq!(set_part(t, OFF, Part::Day, 24), at(2026, 11, 24, 10, 30));
        assert_eq!(set_part(t, OFF, Part::Hour, 7), at(2026, 11, 18, 7, 30));
        assert_eq!(set_part(t, OFF, Part::Minute, 45), at(2026, 11, 18, 10, 45));
    }

    #[test]
    fn a_part_is_cut_to_what_it_allows() {
        let t = at(2026, 11, 18, 10, 30);
        assert_eq!(
            set_part(t, OFF, Part::Day, 31),
            at(2026, 11, 30, 10, 30),
            "November has 30"
        );
        assert_eq!(set_part(t, OFF, Part::Day, 0), at(2026, 11, 1, 10, 30));
        assert_eq!(set_part(t, OFF, Part::Hour, 99), at(2026, 11, 18, 23, 30));
        assert_eq!(set_part(t, OFF, Part::Minute, 75), at(2026, 11, 18, 10, 59));
        let feb = at(2028, 2, 10, 9, 0);
        assert_eq!(
            set_part(feb, OFF, Part::Day, 31),
            at(2028, 2, 29, 9, 0),
            "a leap year"
        );
    }

    #[test]
    fn typed_digits_build_a_number_the_part_can_take() {
        assert_eq!(push_digit("", '1', Part::Hour), "1");
        assert_eq!(push_digit("1", '4', Part::Hour), "14");
        assert_eq!(
            push_digit("4", '5', Part::Hour),
            "5",
            "45 is no hour: start over"
        );
        assert_eq!(
            push_digit("14", '2', Part::Hour),
            "2",
            "a third digit starts over"
        );
        assert_eq!(push_digit("4", '5', Part::Minute), "45");
        assert_eq!(push_digit("3", '5', Part::Day), "5", "35 is no day");
        assert_eq!(push_digit("2", '4', Part::Day), "24");
    }
}
