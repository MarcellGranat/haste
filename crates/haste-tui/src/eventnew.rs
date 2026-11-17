//! The popup that adds a calendar event: the calendar first (the default is "local only", which
//! never leaves this machine), then the title, the start and how long it lasts. Pure state: keys
//! in, a decision out.

use crate::app::Key;
use crate::durfield::DurationEdit;
use crate::eventedit::{EventForm, Field as TimeField};

/// Shortest and longest an event may be (minutes).
const MIN_MINUTES: u32 = 5;
const MAX_MINUTES: u32 = 24 * 60;
/// Longest title typed (characters).
const TITLE_MAX: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Calendar,
    Title,
    Start,
    Duration,
}

/// What a key did to the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Cancel,
    /// Add the event: to this Google calendar, or here only when `None`.
    Create {
        calendar: Option<String>,
        title: String,
        start: i64,
        end: i64,
    },
}

#[derive(Debug, Clone)]
pub struct NewEventForm {
    /// The Google calendars the events already on screen come from; "local only" comes before
    /// them and is the default.
    pub calendars: Vec<String>,
    /// 0 is "local only", then the calendars in order.
    pub picked: usize,
    pub title: String,
    pub field: Field,
    /// The start and the length are edited by the same parts as when an event is moved.
    time: EventForm,
}

impl NewEventForm {
    /// A form that starts at `start` and lasts `minutes`, in a local time `off` seconds ahead
    /// of UTC.
    #[must_use]
    pub fn new(calendars: Vec<String>, start: i64, minutes: u32, off: i64) -> Self {
        let mut time = EventForm::new("", "", start, start + i64::from(minutes) * 60, false, off);
        time.duration = DurationEdit::new(minutes, MIN_MINUTES, MAX_MINUTES, false);
        Self {
            calendars,
            picked: 0,
            title: String::new(),
            field: Field::Calendar,
            time,
        }
    }

    /// The Google calendar picked, `None` for "local only".
    #[must_use]
    pub fn calendar(&self) -> Option<&str> {
        self.picked
            .checked_sub(1)
            .and_then(|i| self.calendars.get(i))
            .map(String::as_str)
    }

    #[must_use]
    pub fn start(&self) -> i64 {
        self.time.start
    }

    #[must_use]
    pub fn end(&self) -> i64 {
        self.time.end()
    }

    #[must_use]
    pub fn start_parts(&self) -> (String, String, String) {
        self.time.start_parts()
    }

    #[must_use]
    pub fn part(&self) -> crate::eventedit::Part {
        self.time.part
    }

    #[must_use]
    pub fn duration(&self) -> &DurationEdit {
        &self.time.duration
    }

    fn go(&mut self, forward: bool) {
        self.field = match (self.field, forward) {
            (Field::Calendar, true) | (Field::Start, false) => Field::Title,
            (Field::Title, true) | (Field::Duration, false) => Field::Start,
            (Field::Start, true) => Field::Duration,
            (Field::Title, false) | (Field::Calendar, false) => Field::Calendar,
            (Field::Duration, true) => Field::Duration,
        };
        match self.field {
            Field::Start => self.time.field = TimeField::Start,
            Field::Duration => self.time.field = TimeField::Duration,
            _ => {}
        }
    }

    fn create(&self) -> Outcome {
        Outcome::Create {
            calendar: self.calendar().map(str::to_owned),
            title: self.title.trim().to_owned(),
            start: self.start(),
            end: self.end(),
        }
    }

    /// One key. Tab and Enter go on to the next field (Enter on the last one adds the event, if
    /// it has a title), Shift-Tab goes back, Esc cancels.
    pub fn handle(&mut self, key: Key) -> Outcome {
        match key {
            Key::Esc => return Outcome::Cancel,
            Key::Tab => self.go(true),
            Key::BackTab => self.go(false),
            Key::Enter => {
                if self.field == Field::Duration {
                    if self.title.trim().is_empty() {
                        self.field = Field::Title;
                    } else {
                        return self.create();
                    }
                } else {
                    self.go(true);
                }
            }
            _ => match self.field {
                Field::Calendar => self.handle_calendar(key),
                Field::Title => self.handle_title(key),
                Field::Start | Field::Duration => {
                    // the time parts take the arrows, the digits and h j k l themselves
                    let _ = self.time.handle(key);
                }
            },
        }
        Outcome::Continue
    }

    fn handle_calendar(&mut self, key: Key) {
        let last = self.calendars.len();
        match key {
            Key::Up | Key::Left | Key::Char('k' | 'h') => {
                self.picked = self.picked.saturating_sub(1);
            }
            Key::Down | Key::Right | Key::Char('j' | 'l') => {
                self.picked = (self.picked + 1).min(last);
            }
            _ => {}
        }
    }

    fn handle_title(&mut self, key: Key) {
        match key {
            Key::Char(c) if !c.is_control() && self.title.chars().count() < TITLE_MAX => {
                self.title.push(c);
            }
            Key::Backspace => {
                self.title.pop();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haste_core::time::from_civil;

    const OFF: i64 = 3_600;

    fn form() -> NewEventForm {
        // Wednesday 18 Nov 2026, 10:00 local, an hour
        let start = from_civil(2026, 11, 18, 10, 0) - OFF;
        NewEventForm::new(vec!["a@x.org".into(), "b@x.org".into()], start, 60, OFF)
    }

    #[test]
    fn it_starts_on_the_calendar_with_local_only_picked() {
        let f = form();
        assert_eq!(f.field, Field::Calendar);
        assert_eq!(f.calendar(), None);
        assert_eq!(f.end() - f.start(), 3_600);
    }

    #[test]
    fn the_calendar_is_picked_with_the_arrows_and_stays_in_range() {
        let mut f = form();
        f.handle(Key::Up);
        assert_eq!(f.calendar(), None, "local only is the first one");
        f.handle(Key::Down);
        assert_eq!(f.calendar(), Some("a@x.org"));
        f.handle(Key::Char('j'));
        f.handle(Key::Char('j'));
        assert_eq!(f.calendar(), Some("b@x.org"), "no wrapping past the last");
        f.handle(Key::Up);
        f.handle(Key::Up);
        assert_eq!(f.calendar(), None);
    }

    #[test]
    fn enter_goes_through_the_fields_and_the_last_one_adds_the_event() {
        let mut f = form();
        assert_eq!(f.handle(Key::Enter), Outcome::Continue);
        assert_eq!(f.field, Field::Title);
        for c in "Dentist".chars() {
            f.handle(Key::Char(c));
        }
        f.handle(Key::Backspace);
        f.handle(Key::Char('t'));
        f.handle(Key::Enter);
        assert_eq!(f.field, Field::Start);
        f.handle(Key::Enter);
        assert_eq!(f.field, Field::Duration);
        let (start, end) = (f.start(), f.end());
        assert_eq!(
            f.handle(Key::Enter),
            Outcome::Create {
                calendar: None,
                title: "Dentist".into(),
                start,
                end
            }
        );
    }

    #[test]
    fn an_event_without_a_title_is_not_added() {
        let mut f = form();
        f.handle(Key::Tab);
        f.handle(Key::Tab);
        f.handle(Key::Tab);
        assert_eq!(f.field, Field::Duration);
        assert_eq!(f.handle(Key::Enter), Outcome::Continue);
        assert_eq!(f.field, Field::Title, "back to the title");
    }

    #[test]
    fn the_start_and_length_change_like_when_an_event_is_moved() {
        let mut f = form();
        f.handle(Key::Tab); // title
        f.handle(Key::Tab); // start
        let start = f.start();
        f.handle(Key::Up);
        assert_eq!(f.start(), start + 86_400, "the day");
        f.handle(Key::Right);
        f.handle(Key::Char('1'));
        f.handle(Key::Char('4'));
        assert_eq!(f.start_parts().1, "14");
        f.handle(Key::Tab); // duration
        f.handle(Key::Up);
        assert_eq!(f.end() - f.start(), 65 * 60, "five minutes at a time");
    }

    #[test]
    fn a_google_calendar_is_returned_by_name_and_escape_cancels() {
        let mut f = form();
        f.handle(Key::Down);
        f.handle(Key::Tab);
        f.handle(Key::Char('x'));
        f.handle(Key::Tab);
        f.handle(Key::Tab);
        let Outcome::Create {
            calendar, title, ..
        } = f.handle(Key::Enter)
        else {
            panic!("expected create");
        };
        assert_eq!(
            (calendar.as_deref(), title.as_str()),
            (Some("a@x.org"), "x")
        );
        assert_eq!(f.handle(Key::Esc), Outcome::Cancel);
    }
}
