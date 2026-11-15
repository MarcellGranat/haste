//! The popup that moves or resizes a calendar event here only: a start (day, hour, minute) and
//! a duration. Google keeps its own times. Pure state: keys in, a decision out.

use crate::app::Key;
use crate::inbox;

const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;
/// Minutes a press changes the minute field, and the duration.
const MINUTE_STEP: i64 = 5;
const DURATION_STEP: i64 = 15;
/// Shortest and longest an event may be (minutes).
const MIN_MINUTES: i64 = 5;
const MAX_MINUTES: i64 = 24 * 60;

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
    pub minutes: i64,
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
            minutes: ((end - start) / 60).clamp(MIN_MINUTES, MAX_MINUTES),
            shifted,
            off,
        }
    }

    /// The end the form would save.
    #[must_use]
    pub fn end(&self) -> i64 {
        self.start + self.minutes * 60
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

    /// The duration as `1 h 30 min`.
    #[must_use]
    pub fn duration_text(&self) -> String {
        match (self.minutes / 60, self.minutes % 60) {
            (0, m) => format!("{m} min"),
            (h, 0) => format!("{h} h"),
            (h, m) => format!("{h} h {m} min"),
        }
    }

    /// One key: Tab switches between the start and the duration, the arrows pick and change,
    /// Enter saves, `R` goes back to Google's times, Esc cancels.
    pub fn handle(&mut self, key: Key) -> Outcome {
        match key {
            Key::Esc => return Outcome::Cancel,
            Key::Enter => return Outcome::Save(self.start, self.end()),
            Key::Char('R') if self.shifted => return Outcome::Reset,
            Key::Tab => {
                self.field = match self.field {
                    Field::Start => Field::Duration,
                    Field::Duration => Field::Start,
                };
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
            }
            Key::Right | Key::Char('l') => {
                self.part = match self.part {
                    Part::Day => Part::Hour,
                    _ => Part::Minute,
                };
            }
            Key::Up | Key::Char('k') | Key::Down | Key::Char('j') => {
                let sign = if matches!(key, Key::Up | Key::Char('k')) {
                    1
                } else {
                    -1
                };
                self.start += sign
                    * match self.part {
                        Part::Day => DAY,
                        Part::Hour => HOUR,
                        Part::Minute => MINUTE_STEP * 60,
                    };
            }
            _ => {}
        }
    }

    fn handle_duration(&mut self, key: Key) {
        let step = match key {
            Key::Up | Key::Char('k') => DURATION_STEP,
            Key::Down | Key::Char('j') => -DURATION_STEP,
            Key::Right | Key::Char('l') => MINUTE_STEP,
            Key::Left | Key::Char('h') => -MINUTE_STEP,
            _ => return,
        };
        self.minutes = (self.minutes + step).clamp(MIN_MINUTES, MAX_MINUTES);
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
        assert_eq!(f.minutes, 90);
        assert_eq!(f.duration_text(), "1 h 30 min");
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
    fn the_duration_changes_in_its_own_field_and_stays_within_bounds() {
        let mut f = form();
        f.handle(Key::Tab);
        assert_eq!(f.field, Field::Duration);
        f.handle(Key::Up);
        assert_eq!(f.minutes, 105);
        f.handle(Key::Left);
        assert_eq!(f.minutes, 100);
        for _ in 0..200 {
            f.handle(Key::Down);
        }
        assert_eq!(f.minutes, MIN_MINUTES);
        for _ in 0..200 {
            f.handle(Key::Up);
        }
        assert_eq!(f.minutes, MAX_MINUTES);
        assert_eq!(f.duration_text(), "24 h");
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
