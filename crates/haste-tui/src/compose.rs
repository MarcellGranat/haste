//! The form behind `n` (note), `l` (label) and the TODO editor: fields made of [`TextArea`]s,
//! attachments chosen with the built-in [`FilePicker`], and the label picker. Pure state; the
//! caller turns a saved form into daemon requests.

use std::path::{Path, PathBuf};

use crate::app::Key;
use crate::editor::{fuzzy, FilePicker, PickerOutcome, TextArea};

/// The biggest file that can be attached (it is copied into haste's own folder).
pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
/// Labels remembered as "recently used".
pub const RECENT_LABELS_MAX: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeKind {
    /// A note about a thread, appended to the notes folder.
    Note { thread_id: String },
    /// A reply to a thread (becomes a draft; only the human sends it).
    Reply { thread_id: String },
    /// A TODO: a new one, or an edit of an existing reminder.
    Todo { reminder_id: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub label: &'static str,
    pub area: TextArea,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compose {
    pub kind: ComposeKind,
    pub heading: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub attachments: Vec<PathBuf>,
    pub picker: Option<FilePicker>,
}

/// What a key did to the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeOutcome {
    Continue,
    Cancel,
    /// `Ctrl-S`: keep it as a draft.
    Save,
    /// `Ctrl-Enter`: send it (a reply goes out after a short delay that `U` can undo).
    Send,
    /// `Ctrl-L`: send it later, at a time chosen in a popup.
    Later,
    /// `Ctrl-G`: edit the focused field in `$EDITOR`.
    External,
    /// `Ctrl-F`: the caller lists files and opens the picker.
    Attach,
}

impl Compose {
    #[must_use]
    pub fn note(thread_id: &str, subject: &str) -> Self {
        Self {
            kind: ComposeKind::Note {
                thread_id: thread_id.to_owned(),
            },
            heading: format!("Note · {subject}"),
            fields: vec![Field {
                label: "Note",
                area: TextArea::new("", true),
            }],
            focus: 0,
            attachments: Vec::new(),
            picker: None,
        }
    }

    #[must_use]
    pub fn reply(thread_id: &str, subject: &str) -> Self {
        Self {
            kind: ComposeKind::Reply {
                thread_id: thread_id.to_owned(),
            },
            heading: format!("Reply · {subject}"),
            fields: vec![Field {
                label:
                    "Reply · Ctrl-S or Ctrl-Enter sends (chat 3 s, mail 10 s) · Ctrl-L later · Esc keeps a draft",
                area: TextArea::new("", true),
            }],
            focus: 0,
            attachments: Vec::new(),
            picker: None,
        }
    }

    /// A TODO form. `due` is the date text shown in the field (empty = no date).
    #[must_use]
    pub fn todo(reminder_id: Option<String>, title: &str, due: &str) -> Self {
        let editing = reminder_id.is_some();
        Self {
            kind: ComposeKind::Todo { reminder_id },
            heading: if editing { "Edit TODO" } else { "New TODO" }.to_owned(),
            fields: vec![
                Field {
                    label: "Title",
                    area: TextArea::new(title, false),
                },
                Field {
                    label: "Due (2026-11-20 09:00, tomorrow, 3h, empty = none)",
                    area: TextArea::new(due, false),
                },
                Field {
                    label: if editing {
                        "Add to the notes (appended, the old notes stay)"
                    } else {
                        "Notes"
                    },
                    area: TextArea::new("", true),
                },
            ],
            focus: 0,
            attachments: Vec::new(),
            picker: None,
        }
    }

    #[must_use]
    pub fn field(&self, label_start: &str) -> Option<&TextArea> {
        self.fields
            .iter()
            .find(|f| f.label.starts_with(label_start))
            .map(|f| &f.area)
    }

    /// Replace the focused field's text (after `$EDITOR`).
    pub fn set_focused_text(&mut self, text: &str) {
        let multiline = self.fields[self.focus].area.lines().len() > 1
            || self.fields[self.focus].label.starts_with("Note")
            || self.fields[self.focus].label.starts_with("Add");
        let text = text.trim_end_matches('\n');
        self.fields[self.focus].area = TextArea::new(text, multiline);
    }

    #[must_use]
    pub fn focused_text(&self) -> String {
        self.fields[self.focus].area.text()
    }

    /// Is there anything written or attached?
    #[must_use]
    pub fn has_content(&self) -> bool {
        !self.attachments.is_empty() || self.fields.iter().any(|f| !f.area.text().trim().is_empty())
    }

    pub fn open_picker(&mut self, files: Vec<PathBuf>) {
        self.picker = Some(FilePicker::new(files));
    }

    pub fn handle(&mut self, key: Key) -> ComposeOutcome {
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
            return ComposeOutcome::Continue;
        }
        match key {
            Key::Esc => return ComposeOutcome::Cancel,
            // Ctrl-Enter is not reported by every terminal (Herdr sends it as a plain Enter), so
            // Ctrl-S, Ctrl-J and Alt-Enter send too
            Key::Ctrl('s') | Key::CtrlEnter | Key::Ctrl('j') => return ComposeOutcome::Send,
            Key::Ctrl('l') => return ComposeOutcome::Later,
            Key::Ctrl('g') => return ComposeOutcome::External,
            Key::Ctrl('f') => return ComposeOutcome::Attach,
            Key::Ctrl('x') => {
                self.attachments.pop();
            }
            Key::Tab => self.focus = (self.focus + 1) % self.fields.len(),
            Key::BackTab => {
                self.focus = (self.focus + self.fields.len() - 1) % self.fields.len();
            }
            Key::Enter if !self.fields[self.focus].area.handle(Key::Enter) => {
                // a one-line field: Enter moves on, and from the last field it saves
                if self.focus + 1 < self.fields.len() {
                    self.focus += 1;
                } else {
                    return ComposeOutcome::Save;
                }
            }
            other => {
                let area = &mut self.fields[self.focus].area;
                // Up/Down leave a one-line field for the neighbour
                if !area.handle(other) {
                    match other {
                        Key::Up if self.focus > 0 => self.focus -= 1,
                        Key::Down if self.focus + 1 < self.fields.len() => self.focus += 1,
                        _ => {}
                    }
                }
            }
        }
        ComposeOutcome::Continue
    }
}

/// A file name that is safe to create: no path parts, no control characters.
#[must_use]
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').trim().to_owned();
    if cleaned.is_empty() {
        "file".to_owned()
    } else {
        cleaned.chars().take(120).collect()
    }
}

/// Copy a chosen file into `<dir>` (0600) and return the copy. Only regular files are taken (a
/// symlink is refused), and nothing bigger than [`MAX_ATTACHMENT_BYTES`].
///
/// # Errors
/// A short reason the file was not attached.
pub fn store_attachment(dir: &Path, src: &Path, stamp: i64) -> Result<PathBuf, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let meta = std::fs::symlink_metadata(src).map_err(|_| "file not readable".to_owned())?;
    if !meta.file_type().is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > MAX_ATTACHMENT_BYTES {
        return Err("file is over 25 MB".into());
    }
    std::fs::create_dir_all(dir).map_err(|_| "cannot create the attachments folder".to_owned())?;
    let name = safe_name(
        &src.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
    );
    let target = dir.join(format!("{stamp}-{name}"));
    let mut from = std::fs::File::open(src).map_err(|_| "file not readable".to_owned())?;
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&target)
        .map_err(|_| "cannot write the copy".to_owned())?;
    std::io::copy(&mut from, &mut to).map_err(|_| "copy failed".to_owned())?;
    Ok(target)
}

/// One row of the label picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelRow {
    pub label: String,
    /// Already on the thread (Enter takes it off).
    pub on: bool,
}

/// Pick a label for a thread: type to narrow; the typed text itself is offered as a new label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelPicker {
    pub thread_id: String,
    /// Candidates, most recently used first.
    pub candidates: Vec<String>,
    pub on: Vec<String>,
    pub input: String,
    pub selected: usize,
}

/// What a key did to the label picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelOutcome {
    Open,
    Closed,
    Toggle { label: String, add: bool },
}

impl LabelPicker {
    #[must_use]
    pub fn new(thread_id: &str, candidates: Vec<String>, on: Vec<String>) -> Self {
        Self {
            thread_id: thread_id.to_owned(),
            candidates,
            on,
            input: String::new(),
            selected: 0,
        }
    }

    /// The rows shown: matching candidates, then the typed text as a new label if it is not one.
    #[must_use]
    pub fn rows(&self) -> Vec<LabelRow> {
        let typed = self.input.trim();
        let mut rows: Vec<LabelRow> = self
            .candidates
            .iter()
            .filter(|c| fuzzy(typed, c))
            .map(|c| LabelRow {
                label: c.clone(),
                on: self.on.contains(c),
            })
            .collect();
        if !typed.is_empty() && !rows.iter().any(|r| r.label == typed) {
            rows.push(LabelRow {
                label: typed.to_owned(),
                on: self.on.iter().any(|t| t == typed),
            });
        }
        rows
    }

    pub fn handle(&mut self, key: Key) -> LabelOutcome {
        match key {
            Key::Esc => return LabelOutcome::Closed,
            Key::Enter => {
                let rows = self.rows();
                if let Some(row) = rows.get(self.selected) {
                    return LabelOutcome::Toggle {
                        label: row.label.clone(),
                        add: !row.on,
                    };
                }
            }
            Key::Down | Key::Tab => {
                let last = self.rows().len().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
            }
            Key::Up | Key::BackTab => self.selected = self.selected.saturating_sub(1),
            Key::Backspace => {
                self.input.pop();
                self.selected = 0;
            }
            Key::Char(c) if !c.is_control() => {
                self.input.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        LabelOutcome::Open
    }
}

/// Candidate labels, best first: the recently used ones, then project folders under `roots`
/// (newest first), then the Herdr workspace labels. Duplicates are dropped.
#[must_use]
pub fn label_candidates(
    recent: &[String],
    roots: &[PathBuf],
    workspace_labels: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |label: &str| {
        let label = label.trim();
        if !label.is_empty() && !out.iter().any(|l| l == label) {
            out.push(label.to_owned());
        }
    };
    for r in recent {
        push(r);
    }
    let mut folders: Vec<(std::time::SystemTime, String)> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            folders.push((modified, name));
        }
    }
    folders.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, name) in &folders {
        push(name);
    }
    for l in workspace_labels {
        push(l);
    }
    out
}

/// Move `label` to the front of the recent list.
#[must_use]
pub fn bump_recent(mut recent: Vec<String>, label: &str) -> Vec<String> {
    recent.retain(|l| l != label);
    recent.insert(0, label.to_owned());
    recent.truncate(RECENT_LABELS_MAX);
    recent
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(c: &mut Compose, keys: &[Key]) -> ComposeOutcome {
        let mut last = ComposeOutcome::Continue;
        for k in keys {
            last = c.handle(*k);
        }
        last
    }

    #[test]
    fn a_todo_form_moves_with_enter_and_saves_from_the_last_field() {
        let mut c = Compose::todo(None, "", "");
        press(&mut c, &[Key::Char('a'), Key::Enter]);
        assert_eq!(c.focus, 1);
        press(&mut c, &[Key::Tab]);
        assert_eq!(c.focus, 2);
        assert_eq!(c.handle(Key::Enter), ComposeOutcome::Continue); // newline in the notes
        assert_eq!(c.handle(Key::Ctrl('s')), ComposeOutcome::Send);
        assert_eq!(c.field("Title").unwrap().text(), "a");
    }

    #[test]
    fn ctrl_chords_and_escape() {
        let mut c = Compose::note("t1", "subject");
        assert_eq!(c.handle(Key::Ctrl('g')), ComposeOutcome::External);
        assert_eq!(c.handle(Key::Ctrl('f')), ComposeOutcome::Attach);
        assert_eq!(c.handle(Key::Esc), ComposeOutcome::Cancel);
    }

    #[test]
    fn the_picker_adds_an_attachment_and_ctrl_x_removes_it() {
        let mut c = Compose::note("t1", "s");
        c.open_picker(vec!["/a/b.pdf".into()]);
        c.handle(Key::Enter);
        assert_eq!(c.attachments, vec![PathBuf::from("/a/b.pdf")]);
        assert!(c.picker.is_none());
        c.handle(Key::Ctrl('x'));
        assert!(c.attachments.is_empty());
    }

    #[test]
    fn external_text_replaces_the_focused_field() {
        let mut c = Compose::note("t1", "s");
        c.set_focused_text("line one\nline two\n");
        assert_eq!(c.focused_text(), "line one\nline two");
    }

    #[test]
    fn labels_come_recent_first_and_a_typed_one_is_offered() {
        let c = label_candidates(
            &["beta".into()],
            &[],
            &["alpha".into(), "beta".into(), "gamma".into()],
        );
        assert_eq!(c, ["beta", "alpha", "gamma"]);
        let mut p = LabelPicker::new("t", c, vec!["alpha".into()]);
        for ch in "zz".chars() {
            p.handle(Key::Char(ch));
        }
        assert_eq!(
            p.handle(Key::Enter),
            LabelOutcome::Toggle {
                label: "zz".into(),
                add: true
            }
        );
        let mut p = LabelPicker::new("t", vec!["alpha".into()], vec!["alpha".into()]);
        assert_eq!(
            p.handle(Key::Enter),
            LabelOutcome::Toggle {
                label: "alpha".into(),
                add: false
            }
        );
    }

    #[test]
    fn recent_labels_move_to_the_front_and_stay_bounded() {
        let r = bump_recent(vec!["a".into(), "b".into()], "b");
        assert_eq!(r, ["b", "a"]);
        let many: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        assert_eq!(bump_recent(many, "x").len(), RECENT_LABELS_MAX);
    }

    #[test]
    fn names_are_made_safe_and_only_regular_small_files_are_copied() {
        assert_eq!(safe_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_name("..."), "file");
        let base = std::env::temp_dir().join(format!("haste-attach-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let src = base.join("a.txt");
        std::fs::write(&src, "hi").unwrap();
        let link = base.join("link.txt");
        let _ = std::os::unix::fs::symlink(&src, &link);
        let dir = base.join("out");
        let copy = store_attachment(&dir, &src, 5).unwrap();
        assert_eq!(std::fs::read_to_string(copy).unwrap(), "hi");
        assert!(store_attachment(&dir, &link, 6).is_err());
        assert!(store_attachment(&dir, &base, 7).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
