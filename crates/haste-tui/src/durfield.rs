//! A length of time typed or stepped: hours or minutes (Left and Right pick the unit), Up and
//! Down step it, digits type it in the picked unit. Used by the popups that set how long an
//! event or a task lasts.

use crate::app::Key;

/// The unit digits are typed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Hours,
    Minutes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationEdit {
    pub minutes: u32,
    pub unit: Unit,
    typed: String,
    min: u32,
    max: u32,
    /// Up and Down walk the offered task lengths; otherwise they add or take an hour (in the
    /// hours unit) or five minutes.
    task_steps: bool,
}

impl DurationEdit {
    #[must_use]
    pub fn new(minutes: u32, min: u32, max: u32, task_steps: bool) -> Self {
        Self {
            minutes: minutes.clamp(min, max),
            unit: Unit::Minutes,
            typed: String::new(),
            min,
            max,
            task_steps,
        }
    }

    /// `1 h 30 min`, `45 min`, `2 h`.
    #[must_use]
    pub fn text(&self) -> String {
        match (self.minutes / 60, self.minutes % 60) {
            (0, m) => format!("{m} min"),
            (h, 0) => format!("{h} h"),
            (h, m) => format!("{h} h {m} min"),
        }
    }

    /// What has been typed so far, for showing.
    #[must_use]
    pub fn typed(&self) -> &str {
        &self.typed
    }

    /// Take a key. Returns whether it was one of the duration's keys.
    pub fn handle(&mut self, key: Key) -> bool {
        match key {
            Key::Left | Key::Right | Key::Char('h' | 'l') => {
                self.unit = match self.unit {
                    Unit::Hours => Unit::Minutes,
                    Unit::Minutes => Unit::Hours,
                };
                self.typed.clear();
            }
            Key::Up | Key::Char('k') | Key::Down | Key::Char('j') => {
                let up = matches!(key, Key::Up | Key::Char('k'));
                self.typed.clear();
                self.minutes = if self.task_steps {
                    haste_proto::next_task_minutes(self.minutes, up)
                } else {
                    let by = match self.unit {
                        Unit::Hours => 60,
                        Unit::Minutes => 5,
                    };
                    if up {
                        self.minutes + by
                    } else {
                        self.minutes.saturating_sub(by)
                    }
                }
                .clamp(self.min, self.max);
            }
            Key::Char(c) if c.is_ascii_digit() => {
                // a third digit starts over, like the date fields
                if self.typed.len() >= 3 {
                    self.typed.clear();
                }
                self.typed.push(c);
                self.apply_typed();
            }
            Key::Backspace => {
                self.typed.pop();
                if !self.typed.is_empty() {
                    self.apply_typed();
                }
            }
            _ => return false,
        }
        true
    }

    /// The typed number, in the picked unit, becomes the length (kept within bounds).
    fn apply_typed(&mut self) {
        let n: u32 = self.typed.parse().unwrap_or(0);
        let minutes = match self.unit {
            Unit::Hours => n.saturating_mul(60),
            Unit::Minutes => n,
        };
        // whole fives: that is what the calendar and the daemon take
        let rounded = (minutes + 2) / 5 * 5;
        self.minutes = rounded.clamp(self.min, self.max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> DurationEdit {
        DurationEdit::new(30, 5, 240, true)
    }

    #[test]
    fn up_and_down_walk_the_task_lengths() {
        let mut d = task();
        for expected in [60, 120, 180, 240, 240] {
            d.handle(Key::Up);
            assert_eq!(d.minutes, expected);
        }
        for expected in [180, 120, 60, 30, 10, 10] {
            d.handle(Key::Down);
            assert_eq!(d.minutes, expected);
        }
        // from a typed length the next step in the list
        d.minutes = 45;
        d.handle(Key::Up);
        assert_eq!(d.minutes, 60);
        d.minutes = 45;
        d.handle(Key::Down);
        assert_eq!(d.minutes, 30);
    }

    #[test]
    fn digits_are_typed_in_the_picked_unit_and_kept_within_bounds() {
        let mut d = task();
        for c in "45".chars() {
            d.handle(Key::Char(c));
        }
        assert_eq!(d.minutes, 45);
        d.handle(Key::Right); // hours
        assert_eq!(d.unit, Unit::Hours);
        d.handle(Key::Char('3'));
        assert_eq!(d.minutes, 180);
        d.handle(Key::Char('9'));
        assert_eq!(d.minutes, 240, "4 hours at most");
        d.handle(Key::Left); // minutes again
        d.handle(Key::Char('1'));
        assert_eq!(d.minutes, 5, "5 minutes at least");
        d.handle(Key::Char('0'));
        assert_eq!(d.minutes, 10);
        // a fourth digit starts over
        let mut d = task();
        for c in "123".chars() {
            d.handle(Key::Char(c));
        }
        assert_eq!(d.minutes, 125, "a typed 123 is rounded to whole fives");
        d.handle(Key::Char('7'));
        assert_eq!(d.minutes, 5, "7 is rounded to 5");
        // Backspace takes a digit back
        let mut d = task();
        d.handle(Key::Char('7'));
        d.handle(Key::Char('5'));
        assert_eq!(d.minutes, 75);
        d.handle(Key::Backspace);
        assert_eq!(d.minutes, 5);
        assert!(!d.handle(Key::Tab));
    }

    #[test]
    fn an_event_length_steps_by_hours_or_five_minutes() {
        let mut d = DurationEdit::new(90, 5, 24 * 60, false);
        d.handle(Key::Up);
        assert_eq!(d.minutes, 95, "minutes unit: five at a time");
        d.handle(Key::Left);
        d.handle(Key::Up);
        assert_eq!(d.minutes, 155, "hours unit: an hour");
        assert_eq!(d.text(), "2 h 35 min");
        d.handle(Key::Char('1'));
        d.handle(Key::Char('2'));
        assert_eq!(d.minutes, 720);
        assert_eq!(d.text(), "12 h");
    }
}
