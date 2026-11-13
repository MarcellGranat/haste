//! Small editing widgets that live inside the TUI: a multi-line text block and a file picker.
//! Both are plain state machines driven by [`Key`]; nothing here touches the terminal, so the
//! TUI never has to suspend for them.

use std::path::{Path, PathBuf};

use crate::app::Key;

/// A multi-line text block with a cursor. `col` counts characters, not bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextArea {
    lines: Vec<String>,
    pub row: usize,
    pub col: usize,
    multiline: bool,
}

impl TextArea {
    #[must_use]
    pub fn new(text: &str, multiline: bool) -> Self {
        let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        if !multiline {
            lines = vec![lines.join(" ")];
        }
        let row = lines.len() - 1;
        let col = lines[row].chars().count();
        Self {
            lines,
            row,
            col,
            multiline,
        }
    }

    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(String::is_empty)
    }

    fn byte_at(&self, col: usize) -> usize {
        let line = &self.lines[self.row];
        line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
    }

    fn len_of(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    /// Apply an editing key. Returns `false` for a key the block does not use.
    pub fn handle(&mut self, key: Key) -> bool {
        match key {
            Key::Char(c) if !c.is_control() => {
                let at = self.byte_at(self.col);
                self.lines[self.row].insert(at, c);
                self.col += 1;
            }
            Key::Enter if self.multiline => {
                let at = self.byte_at(self.col);
                let rest = self.lines[self.row].split_off(at);
                self.lines.insert(self.row + 1, rest);
                self.row += 1;
                self.col = 0;
            }
            Key::Backspace => {
                if self.col > 0 {
                    let end = self.byte_at(self.col);
                    let start = self.byte_at(self.col - 1);
                    self.lines[self.row].replace_range(start..end, "");
                    self.col -= 1;
                } else if self.row > 0 {
                    let line = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.len_of(self.row);
                    self.lines[self.row].push_str(&line);
                }
            }
            Key::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.len_of(self.row);
                }
            }
            Key::Right => {
                if self.col < self.len_of(self.row) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            Key::Up if self.multiline && self.row > 0 => {
                self.row -= 1;
                self.col = self.col.min(self.len_of(self.row));
            }
            Key::Down if self.multiline && self.row + 1 < self.lines.len() => {
                self.row += 1;
                self.col = self.col.min(self.len_of(self.row));
            }
            Key::Ctrl('a') => self.col = 0,
            Key::Ctrl('e') => self.col = self.len_of(self.row),
            Key::Ctrl('u') => {
                let end = self.byte_at(self.col);
                self.lines[self.row].replace_range(..end, "");
                self.col = 0;
            }
            _ => return false,
        }
        true
    }
}

/// Directories skipped while looking for files to attach.
const SKIP_DIRS: &[&str] = &["target", "node_modules", "Library", ".git"];
/// The most files listed, and the deepest folder walked, so the picker opens at once.
pub const PICKER_MAX_FILES: usize = 20_000;
pub const PICKER_MAX_DEPTH: usize = 4;

/// Every file under `roots` (hidden entries and build folders skipped), newest first.
#[must_use]
pub fn scan_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for root in roots {
        walk(root, 0, &mut found);
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.dedup_by(|a, b| a.1 == b.1);
    found.into_iter().map(|(_, p)| p).collect()
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<(std::time::SystemTime, PathBuf)>) {
    if depth > PICKER_MAX_DEPTH || out.len() >= PICKER_MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if !SKIP_DIRS.contains(&name.as_ref()) {
                walk(&path, depth + 1, out);
            }
        } else if kind.is_file() {
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            out.push((modified, path));
        }
    }
}

/// Does `query` match `text` as a case-insensitive subsequence (the way fzf matches)? Every
/// whitespace-separated term must match.
#[must_use]
pub fn fuzzy(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query.split_whitespace().all(|term| {
        let mut chars = text.chars();
        term.to_lowercase().chars().all(|c| chars.any(|t| t == c))
    })
}

/// The file picker: type to narrow, Up/Down to move, Enter to take the file, Esc to close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePicker {
    files: Vec<PathBuf>,
    pub query: String,
    pub selected: usize,
}

/// What a key did to the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerOutcome {
    Open,
    Chosen(PathBuf),
    Closed,
}

impl FilePicker {
    #[must_use]
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files,
            query: String::new(),
            selected: 0,
        }
    }

    /// The files matching the query, best (newest) first.
    #[must_use]
    pub fn matches(&self) -> Vec<&PathBuf> {
        self.files
            .iter()
            .filter(|p| fuzzy(&self.query, &p.to_string_lossy()))
            .take(500)
            .collect()
    }

    pub fn handle(&mut self, key: Key) -> PickerOutcome {
        match key {
            Key::Esc => return PickerOutcome::Closed,
            Key::Enter => {
                // a typed absolute path wins, so a file outside the scanned folders can be attached
                let typed = self.query.trim();
                if typed.starts_with('/') && Path::new(typed).is_file() {
                    return PickerOutcome::Chosen(PathBuf::from(typed));
                }
                if let Some(p) = self.matches().get(self.selected) {
                    return PickerOutcome::Chosen((*p).clone());
                }
            }
            Key::Down | Key::Tab => {
                let last = self.matches().len().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
            }
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            Key::Char(c) if !c.is_control() => {
                self.query.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        PickerOutcome::Open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(area: &mut TextArea, s: &str) {
        for c in s.chars() {
            area.handle(if c == '\n' { Key::Enter } else { Key::Char(c) });
        }
    }

    #[test]
    fn typing_enter_and_backspace_across_lines() {
        let mut a = TextArea::new("", true);
        type_str(&mut a, "héllo\nwörld");
        assert_eq!(a.text(), "héllo\nwörld");
        for _ in 0..5 {
            a.handle(Key::Backspace);
        }
        a.handle(Key::Backspace); // joins the lines
        assert_eq!(a.text(), "héllo");
        a.handle(Key::Left);
        a.handle(Key::Char('X'));
        assert_eq!(a.text(), "héllXo");
    }

    #[test]
    fn a_single_line_field_refuses_newlines() {
        let mut a = TextArea::new("a\nb", false);
        assert_eq!(a.text(), "a b");
        assert!(!a.handle(Key::Enter));
        assert!(!a.handle(Key::Ctrl('s')));
    }

    #[test]
    fn up_and_down_keep_the_column_inside_the_line() {
        let mut a = TextArea::new("long line\nab", true);
        a.handle(Key::Up);
        assert_eq!(a.row, 0);
        a.handle(Key::Ctrl('e'));
        a.handle(Key::Down);
        assert_eq!((a.row, a.col), (1, 2));
    }

    #[test]
    fn fuzzy_matching_is_a_case_insensitive_subsequence() {
        assert!(fuzzy("dl inv", "/Users/x/Downloads/Invoice.pdf"));
        assert!(!fuzzy("zzz", "/Users/x/a.pdf"));
        assert!(fuzzy("", "anything"));
    }

    #[test]
    fn the_picker_narrows_moves_and_chooses() {
        let mut p = FilePicker::new(vec!["/a/one.pdf".into(), "/a/two.txt".into()]);
        for c in "two".chars() {
            p.handle(Key::Char(c));
        }
        assert_eq!(p.matches().len(), 1);
        assert_eq!(
            p.handle(Key::Enter),
            PickerOutcome::Chosen(PathBuf::from("/a/two.txt"))
        );
        assert_eq!(p.handle(Key::Esc), PickerOutcome::Closed);
    }
}
