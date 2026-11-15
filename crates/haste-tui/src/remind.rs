//! The reminder popup (`t` on a thread): a title with a dimmed suggestion, a date picker, a
//! repeat rule, notes, project labels and attachments. Pure state: keys in, a [`Task`] out.

use std::path::PathBuf;

use haste_core::model::{RepeatFreq, RepeatRule};

use crate::app::Key;
use crate::editor::{FilePicker, PickerOutcome};
use crate::{inbox, smartdate};

const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;
/// Minutes a press of Up or Down moves the minute field.
const MINUTE_STEP: i64 = 5;
/// Longest repeat interval the picker offers.
const MAX_EVERY: u32 = 99;

/// The form's fields, in the order Tab visits them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Date,
    Duration,
    Repeat,
    Notes,
    Labels,
    Files,
}

impl Field {
    const ORDER: [Self; 7] = [
        Self::Title,
        Self::Date,
        Self::Duration,
        Self::Repeat,
        Self::Notes,
        Self::Labels,
        Self::Files,
    ];

    fn next(self) -> Self {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }
}

/// Which part of the date Left and Right are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatePart {
    Day,
    Hour,
    Minute,
}

/// What the form is ready to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Set when an existing TODO was edited (it is changed, not created).
    pub reminder_id: Option<String>,
    /// The labels the TODO had before the edit.
    pub tags_before: Vec<String>,
    /// The thread it is about, when it was made from one.
    pub thread_id: Option<String>,
    pub title: String,
    pub due: Option<i64>,
    pub repeat: Option<RepeatRule>,
    /// How long it takes on the calendar, in minutes.
    pub minutes: u32,
    /// How long it took before an edit (the same as `minutes` for a new one).
    pub minutes_before: u32,
    pub notes: String,
    pub tags: Vec<String>,
    pub attachments: Vec<PathBuf>,
}

/// What a key did to the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Cancel,
    Save(Box<Task>),
}

#[derive(Debug, Clone)]
pub struct RemindForm {
    /// The TODO being edited; `None` for a new one.
    pub edit_id: Option<String>,
    tags_before: Vec<String>,
    pub thread_id: Option<String>,
    /// Shown dimmed while the title is empty.
    pub suggestion: String,
    pub title: String,
    pub field: Field,
    pub part: DatePart,
    pub due: Option<i64>,
    pub repeat: Option<RepeatRule>,
    /// How long it takes on the calendar: Left and Right pick the unit, Up and Down step,
    /// digits type it.
    pub duration: crate::durfield::DurationEdit,
    minutes_before: u32,
    /// The digits typed into the picked part of the date.
    date_typed: String,
    pub notes: String,
    /// Known project labels, and whether each is on.
    pub labels: Vec<(String, bool)>,
    /// Index into [`Self::visible_labels`].
    pub label_cursor: usize,
    /// Typed in the Labels field: narrows the labels fuzzily, fzf style. Empty = the arrows.
    pub label_query: String,
    /// The arrows or a query moved the cursor in the Labels field: Enter then picks the
    /// highlighted label before it saves.
    label_navigated: bool,
    pub attachments: Vec<PathBuf>,
    pub picker: Option<FilePicker>,
    /// Byte ranges of the title's words that gave the date or the repeat.
    pub marks: Vec<(usize, usize)>,
    /// The date, or the repeat, was read from the title (and not set by hand).
    pub due_from_title: bool,
    pub repeat_from_title: bool,
    /// The date or the repeat was set by hand: the words of the title no longer change them.
    date_touched: bool,
    repeat_touched: bool,
    /// What the thread's subject says about a date (used when the title says nothing).
    subject_due: Option<i64>,
    /// An edited TODO's date and repeat before the edit: kept when the title says nothing.
    original: Option<(Option<i64>, Option<RepeatRule>)>,
    now: i64,
    off: i64,
}

impl RemindForm {
    /// A form for a thread (or for a free-standing task when `thread_id` is `None`, with an
    /// empty suggestion). `labels` are all known project labels, `on` those the thread has.
    #[must_use]
    pub fn new(
        thread_id: Option<&str>,
        suggestion: &str,
        labels: &[String],
        on: &[String],
        now: i64,
        off: i64,
    ) -> Self {
        let when = smartdate::parse(suggestion, now, off);
        let mut form = Self {
            edit_id: None,
            tags_before: Vec::new(),
            thread_id: thread_id.map(str::to_owned),
            suggestion: suggestion.to_owned(),
            title: String::new(),
            field: Field::Title,
            part: DatePart::Day,
            due: None,
            repeat: None,
            duration: crate::durfield::DurationEdit::new(
                haste_proto::DEFAULT_TASK_MINUTES,
                haste_proto::MIN_TASK_MINUTES,
                haste_proto::MAX_TASK_MINUTES,
                true,
            ),
            minutes_before: haste_proto::DEFAULT_TASK_MINUTES,
            date_typed: String::new(),
            notes: String::new(),
            labels: labels.iter().map(|l| (l.clone(), on.contains(l))).collect(),
            label_cursor: 0,
            label_query: String::new(),
            label_navigated: false,
            attachments: Vec::new(),
            picker: None,
            marks: Vec::new(),
            due_from_title: false,
            repeat_from_title: false,
            date_touched: false,
            repeat_touched: false,
            subject_due: when.due,
            original: None,
            now,
            off,
        };
        form.reread();
        form
    }

    /// A form for an existing TODO, filled with what it has now. The notes field is empty:
    /// what is typed there is added to the notes it already has.
    #[must_use]
    pub fn editing(
        todo: &haste_proto::ReminderView,
        labels: &[String],
        now: i64,
        off: i64,
    ) -> Self {
        let mut all: Vec<String> = labels.to_vec();
        for t in &todo.tags {
            if !all.contains(t) {
                all.push(t.clone());
            }
        }
        let mut form = Self::new(None, "", &all, &todo.tags, now, off);
        form.edit_id = Some(todo.reminder.id.clone());
        form.tags_before = todo.tags.clone();
        form.title = todo.reminder.title.clone();
        form.original = Some((todo.reminder.due_at, todo.repeat));
        form.duration = crate::durfield::DurationEdit::new(
            todo.minutes,
            haste_proto::MIN_TASK_MINUTES,
            haste_proto::MAX_TASK_MINUTES,
            true,
        );
        form.minutes_before = todo.minutes;
        // the words of the title set the date and the repeat (also as it opens); without
        // words the TODO keeps what it has, until it is changed by hand
        form.reread();
        form
    }

    /// The title that would be used: what was typed, else the suggestion.
    #[must_use]
    pub fn effective_title(&self) -> &str {
        if self.title.trim().is_empty() {
            &self.suggestion
        } else {
            &self.title
        }
    }

    /// Read the date and the repeat out of the words, unless they were set by hand.
    fn reread(&mut self) {
        let when = smartdate::parse(&self.title, self.now, self.off);
        if !self.date_touched {
            self.due = match self.original {
                // an edit: the words win, else what the TODO had
                Some((due, _)) => when.due.or(due),
                None => when
                    .due
                    .or(self.subject_due)
                    .or_else(|| Some(inbox::tomorrow_morning(self.now, self.off))),
            };
        }
        if !self.repeat_touched {
            self.repeat = match self.original {
                Some((_, repeat)) => when.repeat.or(repeat),
                None => when.repeat,
            };
        }
        self.due_from_title = !self.date_touched && when.due.is_some();
        self.repeat_from_title = !self.repeat_touched && when.repeat.is_some();
        self.marks = when.marks;
    }

    /// The date as three texts: day, hour, minute.
    #[must_use]
    pub fn date_parts(&self) -> Option<(String, String, String)> {
        let due = self.due?;
        let (_, _, _, h, m) = haste_core::time::to_civil(due + self.off);
        let (_, month, day, _, _) = haste_core::time::to_civil(due + self.off);
        let (year, ..) = haste_core::time::to_civil(due + self.off);
        let label = inbox::day_label(due, self.off);
        let weekday = label.split_whitespace().next().unwrap_or("");
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let month_name = months[usize::try_from(month - 1).unwrap_or(0)];
        Some((
            format!("{weekday} {day} {month_name} {year}"),
            format!("{h:02}"),
            format!("{m:02}"),
        ))
    }

    fn task(&self) -> Task {
        Task {
            reminder_id: self.edit_id.clone(),
            tags_before: self.tags_before.clone(),
            thread_id: self.thread_id.clone(),
            title: self.effective_title().trim().to_owned(),
            due: self.due,
            repeat: self.repeat,
            minutes: self.duration.minutes,
            minutes_before: self.minutes_before,
            notes: self.notes.trim().to_owned(),
            tags: self
                .labels
                .iter()
                .filter(|(_, on)| *on)
                .map(|(l, _)| l.clone())
                .collect(),
            attachments: self.attachments.clone(),
        }
    }

    /// The labels the typed query leaves (all of them without a query), best match first, as
    /// indexes into `labels`. A label matches when the query's letters appear in it in order.
    #[must_use]
    pub fn visible_labels(&self) -> Vec<usize> {
        let query: Vec<char> = self.label_query.to_lowercase().chars().collect();
        if query.is_empty() {
            return (0..self.labels.len()).collect();
        }
        let mut scored: Vec<(usize, usize)> = self
            .labels
            .iter()
            .enumerate()
            .filter_map(|(i, (name, _))| {
                let lower = name.to_lowercase();
                let mut want = query.iter().peekable();
                let mut last = None;
                let mut gaps = 0;
                for (at, c) in lower.chars().enumerate() {
                    if want.peek() == Some(&&c) {
                        if let Some(l) = last {
                            gaps += at - l - 1;
                        }
                        last = Some(at);
                        want.next();
                    }
                }
                // a label that starts with the query comes first, then the tighter matches
                want.peek().is_none().then(|| {
                    let start = usize::from(!lower.starts_with(&self.label_query.to_lowercase()));
                    (i, start * 1000 + gaps)
                })
            })
            .collect();
        scored.sort_by_key(|(i, score)| (*score, *i));
        scored.into_iter().map(|(i, _)| i).collect()
    }

    /// `Ctrl-f`: the caller lists files and opens the picker.
    pub fn open_picker(&mut self, files: Vec<PathBuf>) {
        self.picker = Some(FilePicker::new(files));
    }

    fn step_date(&mut self, up: bool) {
        let sign = if up { 1 } else { -1 };
        let base = self
            .due
            .unwrap_or_else(|| inbox::tomorrow_morning(self.now, self.off));
        let by = match self.part {
            DatePart::Day => DAY,
            DatePart::Hour => HOUR,
            DatePart::Minute => MINUTE_STEP * 60,
        };
        self.due = Some(base + sign * by);
        self.date_touched = true;
    }

    /// A typed digit goes into the picked part of the date; the rest of the date stays.
    fn type_into_date(&mut self, digit: char) {
        let part = match self.part {
            DatePart::Day => crate::datefield::Part::Day,
            DatePart::Hour => crate::datefield::Part::Hour,
            DatePart::Minute => crate::datefield::Part::Minute,
        };
        self.date_typed = crate::datefield::push_digit(&self.date_typed, digit, part);
        let value: i64 = self.date_typed.parse().unwrap_or(0);
        let base = self
            .due
            .unwrap_or_else(|| inbox::tomorrow_morning(self.now, self.off));
        self.due = Some(crate::datefield::set_part(base, self.off, part, value));
        self.date_touched = true;
    }

    fn step_repeat(&mut self, key: Key) {
        const FREQS: [Option<RepeatFreq>; 5] = [
            None,
            Some(RepeatFreq::Day),
            Some(RepeatFreq::Week),
            Some(RepeatFreq::Month),
            Some(RepeatFreq::Year),
        ];
        let at = FREQS
            .iter()
            .position(|f| *f == self.repeat.map(|r| r.freq))
            .unwrap_or(0);
        let every = self.repeat.map_or(1, |r| r.every);
        self.repeat = match key {
            Key::Left | Key::Right => {
                let to = if key == Key::Right {
                    (at + 1).min(FREQS.len() - 1)
                } else {
                    at.saturating_sub(1)
                };
                FREQS[to].map(|freq| RepeatRule { freq, every })
            }
            Key::Up => self.repeat.map(|r| RepeatRule {
                every: (r.every + 1).min(MAX_EVERY),
                ..r
            }),
            Key::Down => self.repeat.map(|r| RepeatRule {
                every: r.every.saturating_sub(1).max(1),
                ..r
            }),
            _ => self.repeat,
        };
        self.repeat_touched = true;
    }

    /// One key. Tab goes to the next field, Enter saves what is there, Esc cancels.
    pub fn handle(&mut self, key: Key) -> Outcome {
        if let Some(picker) = &mut self.picker {
            match picker.handle(key) {
                PickerOutcome::Open => {}
                PickerOutcome::Closed => self.picker = None,
                PickerOutcome::Chosen(path) => {
                    if !self.attachments.contains(&path) {
                        self.attachments.push(path);
                    }
                    self.picker = None;
                }
            }
            return Outcome::Continue;
        }
        match key {
            Key::Esc if self.field == Field::Labels && !self.label_query.is_empty() => {
                self.label_query.clear();
                self.label_cursor = 0;
                return Outcome::Continue;
            }
            Key::Esc => return Outcome::Cancel,
            // in the Labels field, after moving or typing there, Enter picks the highlighted
            // label first (as in fzf); the next Enter saves
            Key::Enter
                if self.field == Field::Labels
                    && (self.label_navigated || !self.label_query.is_empty()) =>
            {
                let shown = self.visible_labels();
                if let Some(&i) = shown.get(self.label_cursor) {
                    self.labels[i].1 = true;
                    self.label_cursor = i;
                }
                self.label_query.clear();
                self.label_navigated = false;
                return Outcome::Continue;
            }
            Key::Enter => {
                if self.effective_title().trim().is_empty() {
                    return Outcome::Continue;
                }
                return Outcome::Save(Box::new(self.task()));
            }
            Key::Tab => {
                // leaving the title with nothing typed takes the suggestion as the title
                if self.field == Field::Title && self.title.trim().is_empty() {
                    self.title = self.suggestion.clone();
                    self.reread();
                }
                self.field = self.field.next();
                self.date_typed.clear();
                return Outcome::Continue;
            }
            _ => {}
        }
        match (self.field, key) {
            (Field::Title, Key::Right) if self.title.is_empty() => {
                // accept the suggestion as text and go on editing it
                self.title = self.suggestion.clone();
                self.reread();
            }
            (Field::Title, Key::Char(c)) if !c.is_control() => {
                self.title.push(c);
                self.reread();
            }
            (Field::Title, Key::Backspace) => {
                self.title.pop();
                self.reread();
            }
            (Field::Date, Key::Left) => {
                self.part = match self.part {
                    DatePart::Minute => DatePart::Hour,
                    _ => DatePart::Day,
                };
                self.date_typed.clear();
            }
            (Field::Date, Key::Right) => {
                self.part = match self.part {
                    DatePart::Day => DatePart::Hour,
                    _ => DatePart::Minute,
                };
                self.date_typed.clear();
            }
            (Field::Date, Key::Up) => {
                self.date_typed.clear();
                self.step_date(true);
            }
            (Field::Date, Key::Down) => {
                self.date_typed.clear();
                self.step_date(false);
            }
            // digits set the picked part: the day of the month, the hour or the minute
            (Field::Date, Key::Char(c)) if c.is_ascii_digit() => self.type_into_date(c),
            (Field::Duration, k) => {
                self.duration.handle(k);
            }
            (Field::Date, Key::Backspace) => {
                self.due = None;
                self.date_touched = true;
            }
            (Field::Repeat, Key::Left | Key::Right | Key::Up | Key::Down) => self.step_repeat(key),
            (Field::Repeat, Key::Backspace) => {
                self.repeat = None;
                self.repeat_touched = true;
            }
            (Field::Notes, Key::Char(c)) if !c.is_control() => self.notes.push(c),
            (Field::Notes, Key::Backspace) => {
                self.notes.pop();
            }
            (Field::Labels, Key::Left | Key::Up) => {
                self.label_cursor = self.label_cursor.saturating_sub(1);
                self.label_navigated = true;
            }
            (Field::Labels, Key::Right | Key::Down) => {
                let last = self.visible_labels().len().saturating_sub(1);
                self.label_cursor = (self.label_cursor + 1).min(last);
                self.label_navigated = true;
            }
            (Field::Labels, Key::Char(' ')) => {
                let shown = self.visible_labels();
                if let Some(&i) = shown.get(self.label_cursor) {
                    self.labels[i].1 = !self.labels[i].1;
                }
                self.label_navigated = false;
            }
            // typing narrows the labels (fzf); deleting it all goes back to the arrows
            (Field::Labels, Key::Char(c)) if !c.is_control() => {
                self.label_query.push(c);
                self.label_cursor = 0;
            }
            (Field::Labels, Key::Backspace) => {
                self.label_query.pop();
                self.label_cursor = 0;
            }
            (Field::Files, Key::Backspace) => {
                self.attachments.pop();
            }
            (Field::Files, Key::Char(' ')) => return Outcome::Continue,
            _ => {}
        }
        Outcome::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haste_core::time::from_civil;

    /// 2026-11-15 12:00 UTC, a Sunday, with a local offset of +1 hour.
    const NOW: i64 = 1_794_744_000;
    const OFF: i64 = 3_600;

    fn at(d: i64, h: i64, m: i64) -> i64 {
        from_civil(2026, 11, d, h, m) - OFF
    }

    fn form(suggestion: &str) -> RemindForm {
        RemindForm::new(
            Some("t1"),
            suggestion,
            &["haste".to_owned(), "work".to_owned()],
            &["work".to_owned()],
            NOW,
            OFF,
        )
    }

    fn type_text(f: &mut RemindForm, text: &str) {
        for c in text.chars() {
            f.handle(Key::Char(c));
        }
    }

    #[test]
    fn the_suggestion_is_dim_until_right_takes_it_as_editable_text() {
        let mut f = form("Quarterly plan");
        assert!(f.title.is_empty());
        assert_eq!(f.effective_title(), "Quarterly plan");
        f.handle(Key::Right);
        assert_eq!(f.title, "Quarterly plan");
        type_text(&mut f, " v2");
        assert_eq!(f.title, "Quarterly plan v2");
        // Right does nothing once there is text
        f.handle(Key::Right);
        assert_eq!(f.title, "Quarterly plan v2");
    }

    #[test]
    fn enter_saves_with_the_suggestion_when_nothing_was_typed() {
        let mut f = form("Quarterly plan");
        let Outcome::Save(task) = f.handle(Key::Enter) else {
            panic!("expected a task")
        };
        assert_eq!(task.title, "Quarterly plan");
        assert_eq!(
            task.due,
            Some(at(16, 10, 0)),
            "tomorrow 09:00 without a date in the words"
        );
        assert_eq!(
            task.tags,
            vec!["work".to_owned()],
            "the thread's label comes along"
        );
        assert_eq!(task.thread_id.as_deref(), Some("t1"));
    }

    #[test]
    fn tab_takes_the_suggestion_as_the_title_and_goes_to_the_date() {
        let mut f = form("Pay rent kedd");
        f.handle(Key::Tab);
        assert_eq!(f.title, "Pay rent kedd");
        assert_eq!(f.field, Field::Date);
        assert_eq!(
            f.due,
            Some(at(17, 10, 0)),
            "the date was read from the words"
        );
    }

    #[test]
    fn a_date_in_the_typed_title_sets_the_date_and_repeat() {
        let mut f = form("Mail");
        type_text(&mut f, "call Ana tomorrow 14:30 every week");
        assert_eq!(f.due, Some(at(16, 14, 30)));
        assert_eq!(
            f.repeat,
            Some(RepeatRule {
                freq: RepeatFreq::Week,
                every: 1
            })
        );
    }

    #[test]
    fn the_words_that_gave_the_date_are_marked_as_they_are_typed() {
        let mut f = form("Mail");
        type_text(&mut f, "call Ana in 3 days");
        assert!(f.due_from_title);
        let shown: Vec<&str> = f.marks.iter().map(|(s, e)| &f.title[*s..*e]).collect();
        assert_eq!(shown, ["3 days"]);
        assert_eq!(f.due, Some(at(18, 10, 0)));
        // a half-typed phrase says nothing yet, and what the words gave goes when they do
        type_text(&mut f, " every");
        assert_eq!(f.repeat, None);
        type_text(&mut f, " month");
        assert_eq!(f.repeat.map(|r| r.freq), Some(RepeatFreq::Month));
        for _ in 0..6 {
            f.handle(Key::Backspace);
        }
        assert_eq!(f.repeat, None, "the repeat went with its words");
    }

    #[test]
    fn the_date_moves_by_day_hour_and_minute_and_stays_once_set_by_hand() {
        let mut f = form("Mail");
        f.handle(Key::Tab);
        f.handle(Key::Tab); // title -> date (title taken), still on Date after one Tab? no: back
        f.field = Field::Date;
        let start = f.due.unwrap();
        f.handle(Key::Up);
        assert_eq!(f.due, Some(start + DAY));
        f.handle(Key::Right);
        f.handle(Key::Down);
        assert_eq!(f.due, Some(start + DAY - HOUR));
        f.handle(Key::Right);
        f.handle(Key::Up);
        assert_eq!(f.due, Some(start + DAY - HOUR + 300));
        f.handle(Key::Left);
        assert_eq!(f.part, DatePart::Hour);
        // typing in the title afterwards no longer overrides what was set by hand
        f.field = Field::Title;
        type_text(&mut f, " tomorrow");
        assert_eq!(f.due, Some(start + DAY - HOUR + 300));
        let (day, hour, minute) = f.date_parts().unwrap();
        assert!(
            day.contains("Nov") && hour.len() == 2 && minute.len() == 2,
            "{day} {hour} {minute}"
        );
    }

    #[test]
    fn the_repeat_is_chosen_with_the_arrows_and_the_interval_with_up_and_down() {
        let mut f = form("Mail");
        f.field = Field::Repeat;
        assert_eq!(f.repeat, None);
        f.handle(Key::Right);
        f.handle(Key::Right);
        assert_eq!(f.repeat.map(|r| r.freq), Some(RepeatFreq::Week));
        f.handle(Key::Up);
        f.handle(Key::Up);
        assert_eq!(f.repeat.map(|r| r.every), Some(3));
        f.handle(Key::Left);
        f.handle(Key::Left);
        assert_eq!(f.repeat, None);
    }

    #[test]
    fn typing_in_the_labels_field_narrows_them_like_fzf_and_deleting_it_goes_back() {
        let mut f = RemindForm::new(
            Some("t1"),
            "Mail",
            &[
                "haste".into(),
                "work".into(),
                "home-lab".into(),
                "wow".into(),
            ],
            &[],
            NOW,
            OFF,
        );
        f.field = Field::Labels;
        assert_eq!(
            f.visible_labels(),
            vec![0, 1, 2, 3],
            "the arrows over all of them"
        );
        f.handle(Key::Char('w'));
        assert_eq!(f.visible_labels(), vec![1, 3], "work and wow start with w");
        f.handle(Key::Char('r'));
        assert_eq!(f.visible_labels(), vec![1], "w..r is only work");
        f.handle(Key::Char(' '));
        assert!(f.labels[1].1, "Space toggles the highlighted match");
        f.handle(Key::Backspace);
        f.handle(Key::Backspace);
        assert_eq!(
            f.visible_labels(),
            vec![0, 1, 2, 3],
            "empty again: all labels"
        );
        for c in "hl".chars() {
            f.handle(Key::Char(c));
        }
        assert_eq!(f.visible_labels(), vec![2], "h...l matches home-lab");
        // Esc clears the query first and keeps the form
        assert_eq!(f.handle(Key::Esc), Outcome::Continue);
        assert!(f.label_query.is_empty());
        assert_eq!(f.handle(Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn enter_in_the_labels_field_picks_the_highlighted_label_before_it_saves() {
        let mut f = form("Mail"); // labels: haste, work (work is on)
        f.field = Field::Labels;
        // untouched: Enter just saves, nothing is picked on the way
        let Outcome::Save(task) = f.handle(Key::Enter) else {
            panic!("expected a task")
        };
        assert_eq!(task.tags, vec!["work".to_owned()]);
        // typing narrows to haste; Enter picks it, the next Enter saves
        f.field = Field::Labels;
        f.handle(Key::Char('h'));
        assert_eq!(f.handle(Key::Enter), Outcome::Continue);
        assert!(f.label_query.is_empty(), "the query is done");
        assert!(f.labels[0].1, "haste is on now");
        let Outcome::Save(task) = f.handle(Key::Enter) else {
            panic!("the second Enter saves")
        };
        assert_eq!(task.tags, vec!["haste".to_owned(), "work".to_owned()]);
        // moving with the arrows counts the same way
        let mut f = form("Mail");
        f.field = Field::Labels;
        f.handle(Key::Right); // onto work (already on)
        f.handle(Key::Left); // onto haste
        assert_eq!(f.handle(Key::Enter), Outcome::Continue);
        assert!(f.labels[0].1);
    }

    #[test]
    fn labels_toggle_with_space_and_files_are_attached_with_the_picker() {
        let mut f = form("Mail");
        f.field = Field::Labels;
        f.handle(Key::Char(' ')); // haste on
        f.handle(Key::Right);
        f.handle(Key::Char(' ')); // work off
        f.field = Field::Notes;
        type_text(&mut f, "bring the contract");
        f.open_picker(vec!["/a/b.pdf".into()]);
        f.handle(Key::Enter);
        assert!(f.picker.is_none());
        assert_eq!(f.attachments, vec![PathBuf::from("/a/b.pdf")]);
        let Outcome::Save(task) = f.handle(Key::Enter) else {
            panic!("expected a task")
        };
        assert_eq!(task.tags, vec!["haste".to_owned()]);
        assert_eq!(task.notes, "bring the contract");
        assert_eq!(task.attachments.len(), 1);
    }

    #[test]
    fn editing_starts_from_what_the_todo_has_and_keeps_it_until_changed() {
        let todo = haste_proto::ReminderView {
            reminder: haste_core::model::Reminder {
                id: "r1".into(),
                external_id: "x1".into(),
                title: "Pay rent".into(),
                notes_snippet: "old notes".into(),
                due_at: Some(at(20, 8, 15)),
                completed: false,
                list_name: "L".into(),
                remote_modified_at: 0,
                missing_since: None,
                version: 1,
            },
            tags: vec!["work".into()],
            repeat: Some(RepeatRule {
                freq: RepeatFreq::Month,
                every: 1,
            }),
            minutes: 30,
        };
        let mut f = RemindForm::editing(&todo, &["haste".into()], NOW, OFF);
        assert_eq!(f.title, "Pay rent");
        assert_eq!(f.due, Some(at(20, 8, 15)));
        assert_eq!(f.repeat.map(|r| r.freq), Some(RepeatFreq::Month));
        // the labels it has are on, and one it had that is no longer known is still listed
        assert!(f.labels.iter().any(|(l, on)| l == "work" && *on));
        assert!(!f.due_from_title);
        // words typed in the title move the date, as for a new one
        type_text(&mut f, " tomorrow 11:00");
        assert_eq!(f.due, Some(at(16, 11, 0)));
        assert!(f.due_from_title);
        // and without them it goes back to what it had
        for _ in 0..15 {
            f.handle(Key::Backspace);
        }
        assert_eq!(f.due, Some(at(20, 8, 15)));
        assert_eq!(f.repeat.map(|r| r.freq), Some(RepeatFreq::Month));
        type_text(&mut f, " tomorrow");
        f.field = Field::Notes;
        type_text(&mut f, "bring ID");
        let Outcome::Save(task) = f.handle(Key::Enter) else {
            panic!("expected a task")
        };
        assert_eq!(task.reminder_id.as_deref(), Some("r1"));
        assert_eq!(task.minutes, 30, "the length it has now");
        assert_eq!(task.tags_before, vec!["work".to_owned()]);
        assert_eq!(task.title, "Pay rent tomorrow");
        assert_eq!(task.due, Some(at(16, 10, 0)));
        assert_eq!(task.notes, "bring ID");
        assert!(task.thread_id.is_none());
    }

    #[test]
    fn digits_type_the_picked_part_of_the_date() {
        let mut f = form("Mail"); // tomorrow, Mon 16 Nov, 10:00
        f.handle(Key::Tab); // title taken, on the date
        assert_eq!(f.field, Field::Date);
        // the day: 2 then 4 is the 24th
        f.handle(Key::Char('2'));
        f.handle(Key::Char('4'));
        let (day, _, _) = f.date_parts().unwrap();
        assert!(day.contains("24 Nov"), "{day}");
        // the hour: 1 then 4; the minute: 3 then 0
        f.handle(Key::Right);
        f.handle(Key::Char('1'));
        f.handle(Key::Char('4'));
        f.handle(Key::Right);
        f.handle(Key::Char('3'));
        f.handle(Key::Char('0'));
        let (day, h, m) = f.date_parts().unwrap();
        assert!(day.contains("24 Nov"));
        assert_eq!((h.as_str(), m.as_str()), ("14", "30"));
        // typing in the title afterwards does not move what was set by hand
        f.field = Field::Title;
        type_text(&mut f, " tomorrow");
        assert_eq!(f.date_parts().unwrap().1, "14");
    }

    #[test]
    fn the_length_is_a_field_of_its_own_stepped_or_typed() {
        let mut f = form("Mail");
        f.handle(Key::Tab); // date
        f.handle(Key::Tab); // length
        assert_eq!(f.field, Field::Duration);
        assert_eq!(f.duration.minutes, 30, "30 minutes by default");
        f.handle(Key::Up);
        assert_eq!(f.duration.minutes, 60);
        f.handle(Key::Down);
        f.handle(Key::Down);
        assert_eq!(f.duration.minutes, 10, "10 minutes is possible");
        for c in "25".chars() {
            f.handle(Key::Char(c));
        }
        assert_eq!(f.duration.minutes, 25);
        let Outcome::Save(task) = f.handle(Key::Enter) else {
            panic!("expected a task")
        };
        assert_eq!((task.minutes, task.minutes_before), (25, 30));
    }

    #[test]
    fn escape_cancels_and_an_empty_title_is_not_saved() {
        let mut f = form("");
        assert_eq!(f.handle(Key::Enter), Outcome::Continue, "no title at all");
        assert_eq!(f.handle(Key::Esc), Outcome::Cancel);
    }
}
