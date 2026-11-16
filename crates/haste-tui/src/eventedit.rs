//! The popup that moves or resizes a calendar event here only: a start (day, hour, minute) and
//! a duration. Google keeps its own times. Pure state: keys in, a decision out.

use crate::app::Key;
use crate::durfield::DurationEdit;
use crate::inbox;

const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;
/// Minutes a press changes the minute field.
const MINUTE_STEP: i64 = 5;
/// Shortest and longest an event may be (minutes).
const MIN_MINUTES: u32 = 5;
const MAX_MINUTES: u32 = 24 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Start,
    Duration,
}

/// Which part of the start Left and Right are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Day,
    Hour,
    Minute,
}

/// What a key did to the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Cancel,
    /// Keep the event at these times (start, end), here only.
    Save(i64, i64),
    /// Put it back to the times Google has.
    Reset,
}

#[derive(Debug, Clone)]
pub struct EventForm {
    pub event_id: String,
    pub title: String,
    pub field: Field,
    pub part: Part,
    pub start: i64,
    /// How long it lasts: Left and Right pick the unit, Up and Down step, digits type it.
    pub duration: DurationEdit,
    /// The digits typed into the picked part of the start.
    typed: String,
    /// It was moved here before, so a reset has something to undo.
    pub shifted: bool,
    off: i64,
}

impl EventForm {
    /// A form for an event with these times (unix seconds) in a local time `off` seconds ahead
    /// of UTC.
    #[must_use]
    pub fn new(event_id: &str, title: &str, start: i64, end: i64, shifted: bool, off: i64) -> Self {
        Self {
            event_id: event_id.to_owned(),
            title: title.to_owned(),
            field: Field::Start,
            part: Part::Day,
            start,
            duration: DurationEdit::new(
                u32::try_from((end - start) / 60).unwrap_or(MIN_MINUTES),
                MIN_MINUTES,
                MAX_MINUTES,
                false,
            ),
            typed: String::new(),
            shifted,
            off,
        }
    }

    /// The end the form would save.
    #[must_use]
    pub fn end(&self) -> i64 {
        self.start + i64::from(self.duration.minutes) * 60
    }

    /// The start as three texts: day, hour, minute.
    #[must_use]
    pub fn start_parts(&self) -> (String, String, String) {
        let (_, _, _, h, m) = haste_core::time::to_civil(self.start + self.off);
        (
            inbox::day_label(self.start, self.off),
            format!("{h:02}"),
            format!("{m:02}"),
        )
    }

    /// One key: Tab switches between the start and the duration, the arrows pick and change,
    /// Enter saves, `R` goes back to Google's times, Esc cancels.
    pub fn handle(&mut self, key: Key) -> Outcome {
        match key {
            Key::Esc => return Outcome::Cancel,
            Key::Enter => return Outcome::Save(self.start, self.end()),
            Key::Char('R') if self.shifted => return Outcome::Reset,
            Key::Tab | Key::BackTab => {
                self.field = match self.field {
                    Field::Start => Field::Duration,
                    Field::Duration => Field::Start,
                };
                self.typed.clear();
            }
            _ => match self.field {
                Field::Start => self.handle_start(key),
                Field::Duration => self.handle_duration(key),
            },
        }
        Outcome::Continue
    }

    fn handle_start(&mut self, key: Key) {
        match key {
            Key::Left | Key::Char('h') => {
                self.part = match self.part {
                    Part::Minute => Part::Hour,
                    _ => Part::Day,
                };
                self.typed.clear();
            }
            Key::Right | Key::Char('l') => {
                self.part = match self.part {
                    Part::Day => Part::Hour,
                    _ => Part::Minute,
                };
                self.typed.clear();
            }
            Key::Up | Key::Char('k') | Key::Down | Key::Char('j') => {
                let sign = if matches!(key, Key::Up | Key::Char('k')) {
                    1
                } else {
                    -1
                };
                self.typed.clear();
                self.start += sign
                    * match self.part {
                        Part::Day => DAY,
                        Part::Hour => HOUR,
                        Part::Minute => MINUTE_STEP * 60,
                    };
            }
            // digits set the picked part: the day of the month, the hour or the minute
            Key::Char(c) if c.is_ascii_digit() => {
                let part = match self.part {
                    Part::Day => crate::datefield::Part::Day,
                    Part::Hour => crate::datefield::Part::Hour,
                    Part::Minute => crate::datefield::Part::Minute,
                };
                self.typed = crate::datefield::push_digit(&self.typed, c, part);
                let value: i64 = self.typed.parse().unwrap_or(0);
                self.start = crate::datefield::set_part(self.start, self.off, part, value);
            }
            _ => {}
        }
    }

    fn handle_duration(&mut self, key: Key) {
        self.duration.handle(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haste_core::time::from_civil;

    const OFF: i64 = 3_600;

    fn form() -> EventForm {
        // Wednesday 18 Nov 2026, 10:00 to 11:30 local
        let start = from_civil(2026, 11, 18, 10, 0) - OFF;
        EventForm::new("e1", "Standup", start, start + 90 * 60, false, OFF)
    }

    #[test]
    fn the_form_starts_from_the_events_times() {
        let f = form();
        let (day, h, m) = f.start_parts();
        assert_eq!(
            (day.as_str(), h.as_str(), m.as_str()),
            ("Wed 18 Nov", "10", "00")
        );
        assert_eq!(f.duration.minutes, 90);
        assert_eq!(f.duration.text(), "1 h 30 min");
        assert_eq!(f.end(), f.start + 90 * 60);
    }

    #[test]
    fn the_start_moves_by_day_hour_and_minute() {
        let mut f = form();
        let start = f.start;
        f.handle(Key::Up);
        assert_eq!(f.start, start + DAY);
        f.handle(Key::Right);
        f.handle(Key::Down);
        assert_eq!(f.start, start + DAY - HOUR);
        f.handle(Key::Right);
        f.handle(Key::Up);
        assert_eq!(f.start, start + DAY - HOUR + 300);
        // the end moves with it: the duration is kept
        assert_eq!(f.end() - f.start, 90 * 60);
    }

    #[test]
    fn digits_type_the_day_the_hour_or_the_minute_and_the_end_follows() {
        let mut f = form(); // Wed 18 Nov 10:00, 90 minutes
                            // the day: 2 then 4 is the 24th
        f.handle(Key::Char('2'));
        f.handle(Key::Char('4'));
        assert_eq!(f.start_parts().0, "Tue 24 Nov");
        // the hour: 1 then 4 is 14
        f.handle(Key::Right);
        f.handle(Key::Char('1'));
        f.handle(Key::Char('4'));
        assert_eq!(f.start_parts().1, "14");
        // the minute: 4 then 5 is 45
        f.handle(Key::Right);
        f.handle(Key::Char('4'));
        f.handle(Key::Char('5'));
        assert_eq!(f.start_parts().2, "45");
        assert_eq!(f.end() - f.start, 90 * 60, "the end follows the start");
        let (_, h, m) = f.start_parts();
        assert_eq!((h.as_str(), m.as_str()), ("14", "45"));
    }

    #[test]
    fn the_duration_is_typed_or_stepped_in_its_own_field() {
        let mut f = form();
        f.handle(Key::Tab);
        assert_eq!(f.field, Field::Duration);
        f.handle(Key::Up);
        assert_eq!(f.duration.minutes, 95, "minutes: five at a time");
        f.handle(Key::Left); // hours
        f.handle(Key::Char('3'));
        assert_eq!(f.duration.minutes, 180);
        f.handle(Key::Char('9'));
        f.handle(Key::Char('9'));
        assert_eq!(f.duration.minutes, MAX_MINUTES, "a day at most");
        f.handle(Key::Right); // minutes
        f.handle(Key::Char('1'));
        assert_eq!(f.duration.minutes, MIN_MINUTES, "five minutes at least");
        assert_eq!(f.end(), f.start + 5 * 60);
    }

    #[test]
    fn enter_saves_escape_cancels_and_r_resets_only_a_moved_event() {
        let mut f = form();
        let (s, e) = (f.start, f.end());
        assert_eq!(
            f.handle(Key::Char('R')),
            Outcome::Continue,
            "nothing to undo"
        );
        assert_eq!(f.handle(Key::Enter), Outcome::Save(s, e));
        assert_eq!(f.handle(Key::Esc), Outcome::Cancel);
        f.shifted = true;
        assert_eq!(f.handle(Key::Char('R')), Outcome::Reset);
    }
}
