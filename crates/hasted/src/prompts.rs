//! The prompt templates of the AI helpers, as plain text files the user can edit:
//! `<data-dir>/prompts/draft.txt`, `todo.txt`, `rewrite.txt` and `rewrite_modes.txt`, plus
//! `draft_chat.txt` and `todo_chat.txt` for chats (WhatsApp, Messenger, iMessage, Google Chat).
//!
//! The daemon writes the built-in defaults when a file is missing and never overwrites an edited
//! one. A file is read again when its modification time changes (no restart). A file that is
//! empty, unreadable or lacks a required placeholder is replaced by the built-in default for as
//! long as it stays broken, with one warning that names the file and the reason, never its text.
//!
//! Placeholders (in `draft.txt`, `todo.txt`, `rewrite.txt` and the two chat files):
//! * `{{memories}}`: the notes of the matching memories (empty when none); required.
//! * `{{today}}`: today's date as YYYY-MM-DD; required in `todo.txt` and `todo_chat.txt`.
//! * `{{instruction}}`: what the rewrite mode asks for; required in `rewrite.txt`.
//!
//! `rewrite_modes.txt` has one `mode: instruction` line per mode (`grammar`, `formal`, `fluent`,
//! `friendly`); `#` starts a comment line. A mode that is missing or empty uses its default.

use std::collections::HashMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use haste_core::ai::RewriteMode;
use haste_core::log;

// ---------------------------------------------------------------------------------------------
// CONSTANTS

/// The folder in the data directory.
pub const PROMPTS_DIR: &str = "prompts";
pub const MEMORIES_PLACEHOLDER: &str = "{{memories}}";
pub const TODAY_PLACEHOLDER: &str = "{{today}}";
pub const INSTRUCTION_PLACEHOLDER: &str = "{{instruction}}";
/// The modes file.
pub const MODES_FILE: &str = "rewrite_modes.txt";

/// Default of `draft.txt`.
pub const DEFAULT_DRAFT: &str = "You write the reply of the user (\"Me\") to the latest email. \
Write only the body of the reply, without a subject line, without explanations and without \
placeholders. Keep it short and polite.\n\
Language: reply in the language of the latest message. The text is Hungarian or English. Hungarian \
is often written without accents (for example \"koszonom szepen\" for \"köszönöm szépen\"): \
understand it anyway and write the reply in correct Hungarian with all accents \
(á é í ó ö ő ú ü ű).\n\
{{memories}}";

/// Default of `todo.txt`.
pub const DEFAULT_TODO: &str = "Decide whether the latest email asks the user (\"Me\") to do \
something that should become a task. has_task is false for newsletters, notices, receipts and \
anything that needs no action. title is a short imperative phrase (at most 120 characters). notes \
holds the key details (at most 500 characters). due is a date as YYYY-MM-DD when the mail states or \
implies a deadline (today is {{today}}), else null. priority is none, low, medium or high.\n\
Language: write title and notes in the language of the latest message. The text is Hungarian or \
English. Hungarian is often written without accents (for example \"koszonom szepen\" for \
\"köszönöm szépen\"): understand it anyway and write title and notes in correct Hungarian with all \
accents (á é í ó ö ő ú ü ű).\n\
{{memories}}";

/// Default of `draft_chat.txt`.
pub const DEFAULT_DRAFT_CHAT: &str = "You write the reply of the user (\"Me\") to the latest \
message of a chat (WhatsApp, Messenger, iMessage or similar). Write only the text of the message, \
without explanations and without placeholders. Chats are short and informal: one to three short \
sentences, no greeting line, no sign-off, an emoji only if the other person uses them.\n\
Language: reply in the language of the latest message. The text is Hungarian or English. Hungarian \
is often written without accents (for example \"koszonom szepen\" for \"köszönöm szépen\"): \
understand it anyway and write the reply in correct Hungarian with all accents \
(á é í ó ö ő ú ü ű).\n\
{{memories}}";

/// Default of `todo_chat.txt`.
pub const DEFAULT_TODO_CHAT: &str = "Decide whether the latest message of a chat asks the user \
(\"Me\") to do something that should become a task, or contains a commitment or an appointment \
the user must remember. has_task is false for small talk, thanks, greetings and anything that \
needs no action. title is a short imperative phrase (at most 120 characters). notes holds the key \
details (at most 500 characters). due is a date as YYYY-MM-DD when the message states or implies a \
deadline (today is {{today}}), else null. priority is none, low, medium or high.\n\
Language: write title and notes in the language of the latest message. The text is Hungarian or \
English. Hungarian is often written without accents (for example \"koszonom szepen\" for \
\"köszönöm szépen\"): understand it anyway and write title and notes in correct Hungarian with all \
accents (á é í ó ö ő ú ü ű).\n\
{{memories}}";

/// Default of `rewrite.txt`.
pub const DEFAULT_REWRITE: &str = "You edit the draft reply of the user (\"Me\"). \
Instruction: {{instruction}}\n\
Return only the edited text of the reply, nothing else: no quotes, no comments.\n\
Language: keep the language of the draft. The draft is Hungarian or English. Hungarian is often \
written without accents (for example \"koszonom szepen\" for \"köszönöm szépen\"): whatever the \
instruction, also repair the missing accents and write correct Hungarian with all accents \
(á é í ó ö ő ú ü ű).\n\
{{memories}}";

/// Default of `rewrite_modes.txt`.
pub const DEFAULT_MODES: &str = "# One line per mode: `mode: instruction`. Lines starting with # are comments.\n\
# The custom command of the popup is typed by you each time and is not listed here.\n\
grammar: Fix grammar, spelling and punctuation, and restore missing Hungarian accents. Keep the wording, tone and meaning.\n\
formal: Rewrite in a more formal, polite tone, and restore missing Hungarian accents. Keep the meaning.\n\
fluent: Rewrite so that it reads more fluently and naturally, and restore missing Hungarian accents. Keep the meaning and tone.\n\
friendly: Rewrite in a warmer, friendlier tone, and restore missing Hungarian accents. Keep the meaning.\n";

// ---------------------------------------------------------------------------------------------

/// Which template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Template {
    Draft,
    Todo,
    DraftChat,
    TodoChat,
    Rewrite,
    Modes,
}

impl Template {
    #[must_use]
    pub fn file(self) -> &'static str {
        match self {
            Self::Draft => "draft.txt",
            Self::Todo => "todo.txt",
            Self::DraftChat => "draft_chat.txt",
            Self::TodoChat => "todo_chat.txt",
            Self::Rewrite => "rewrite.txt",
            Self::Modes => MODES_FILE,
        }
    }

    #[must_use]
    pub fn default_text(self) -> &'static str {
        match self {
            Self::Draft => DEFAULT_DRAFT,
            Self::Todo => DEFAULT_TODO,
            Self::DraftChat => DEFAULT_DRAFT_CHAT,
            Self::TodoChat => DEFAULT_TODO_CHAT,
            Self::Rewrite => DEFAULT_REWRITE,
            Self::Modes => DEFAULT_MODES,
        }
    }

    fn required(self) -> &'static [&'static str] {
        match self {
            Self::Draft | Self::DraftChat => &[MEMORIES_PLACEHOLDER],
            Self::Todo | Self::TodoChat => &[MEMORIES_PLACEHOLDER, TODAY_PLACEHOLDER],
            Self::Rewrite => &[MEMORIES_PLACEHOLDER, INSTRUCTION_PLACEHOLDER],
            Self::Modes => &[],
        }
    }
}

/// The key of a mode in `rewrite_modes.txt`; `Custom` has none.
#[must_use]
pub fn mode_key(mode: RewriteMode) -> Option<&'static str> {
    match mode {
        RewriteMode::Custom => None,
        RewriteMode::Grammar => Some("grammar"),
        RewriteMode::Formal => Some("formal"),
        RewriteMode::Fluent => Some("fluent"),
        RewriteMode::Friendly => Some("friendly"),
    }
}

/// `mode: instruction` lines into a map; comments and blank lines are skipped.
#[must_use]
pub fn parse_modes(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once(':')?;
            let value = value.trim();
            (!value.is_empty()).then(|| (key.trim().to_lowercase(), value.to_owned()))
        })
        .collect()
}

/// Why a file cannot be used, as a fixed word.
fn problem(template: Template, text: &str) -> Option<&'static str> {
    if text.trim().is_empty() {
        return Some("empty");
    }
    if template
        .required()
        .iter()
        .any(|placeholder| !text.contains(placeholder))
    {
        return Some("missing placeholder");
    }
    None
}

/// The loader: files in `<data-dir>/prompts/`, cached by modification time.
pub struct PromptStore {
    dir: PathBuf,
    cache: Mutex<HashMap<Template, (Option<SystemTime>, String)>>,
    /// The templates whose broken file was already warned about, with the stamp that was seen.
    warned: Mutex<HashMap<Template, Option<SystemTime>>>,
}

impl PromptStore {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(PROMPTS_DIR),
            cache: Mutex::new(HashMap::new()),
            warned: Mutex::new(HashMap::new()),
        }
    }

    /// Write the default of every template that has no file yet (never overwrites).
    pub fn ensure_defaults(&self) {
        if DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .is_err()
        {
            return;
        }
        for template in [
            Template::Draft,
            Template::Todo,
            Template::DraftChat,
            Template::TodoChat,
            Template::Rewrite,
            Template::Modes,
        ] {
            let path = self.dir.join(template.file());
            if let Ok(mut f) = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                let _ = f.write_all(template.default_text().as_bytes());
            }
        }
    }

    fn stamp(&self, template: Template) -> Option<SystemTime> {
        fs::metadata(self.dir.join(template.file()))
            .and_then(|m| m.modified())
            .ok()
    }

    /// The text to use: the file when it is fine, else the built-in default.
    #[must_use]
    pub fn text(&self, template: Template) -> String {
        let stamp = self.stamp(template);
        if let Ok(cache) = self.cache.lock() {
            if let Some((seen, text)) = cache.get(&template) {
                if *seen == stamp {
                    return text.clone();
                }
            }
        }
        let text = self.read(template, stamp);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(template, (stamp, text.clone()));
        }
        text
    }

    fn read(&self, template: Template, stamp: Option<SystemTime>) -> String {
        let default = template.default_text();
        let reason = match fs::read_to_string(self.dir.join(template.file())) {
            Ok(text) => match problem(template, &text) {
                None => return text,
                Some(reason) => reason,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return default.to_owned(),
            Err(_) => "unreadable",
        };
        let first_time = self
            .warned
            .lock()
            .map(|mut w| w.insert(template, stamp) != Some(stamp))
            .unwrap_or(true);
        if first_time {
            // the file name and a fixed reason, never the text
            log::event(
                log::Level::Warn,
                "apfel",
                "prompt_file",
                &[
                    log::l("file", template.file()),
                    log::l("problem", reason),
                    log::l("using", "built-in default"),
                ],
            );
        }
        default.to_owned()
    }

    /// The instruction of a rewrite mode: the user's line for `Custom`, else the modes file's
    /// line, else the built-in default of that mode.
    #[must_use]
    pub fn instruction(&self, mode: RewriteMode, custom: &str) -> String {
        let Some(key) = mode_key(mode) else {
            return custom.trim().to_owned();
        };
        parse_modes(&self.text(Template::Modes))
            .remove(key)
            .or_else(|| parse_modes(DEFAULT_MODES).remove(key))
            .unwrap_or_else(|| mode.instruction(custom))
    }

    /// The `draft` system prompt for these notes.
    #[must_use]
    pub fn draft(&self, memories: &str) -> String {
        fill(
            &self.text(Template::Draft),
            &[(MEMORIES_PLACEHOLDER, memories)],
        )
    }

    /// The `todo` system prompt for these notes and this date.
    #[must_use]
    pub fn todo(&self, memories: &str, today: &str) -> String {
        fill(
            &self.text(Template::Todo),
            &[(MEMORIES_PLACEHOLDER, memories), (TODAY_PLACEHOLDER, today)],
        )
    }

    /// The `draft` system prompt for a chat.
    #[must_use]
    pub fn draft_chat(&self, memories: &str) -> String {
        fill(
            &self.text(Template::DraftChat),
            &[(MEMORIES_PLACEHOLDER, memories)],
        )
    }

    /// The `todo` system prompt for a chat.
    #[must_use]
    pub fn todo_chat(&self, memories: &str, today: &str) -> String {
        fill(
            &self.text(Template::TodoChat),
            &[(MEMORIES_PLACEHOLDER, memories), (TODAY_PLACEHOLDER, today)],
        )
    }

    /// The `rewrite` system prompt for this instruction and these notes.
    #[must_use]
    pub fn rewrite(&self, memories: &str, instruction: &str) -> String {
        fill(
            &self.text(Template::Rewrite),
            &[
                (MEMORIES_PLACEHOLDER, memories),
                (INSTRUCTION_PLACEHOLDER, instruction),
            ],
        )
    }
}

/// Replace each placeholder once over the template (the values are not searched again).
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match values.iter().find(|(name, _)| tail.starts_with(name)) {
            Some((name, value)) => {
                out.push_str(value);
                rest = &tail[name.len()..];
            }
            None => {
                out.push_str("{{");
                rest = &tail[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, PromptStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = PromptStore::new(dir.path());
        (dir, s)
    }

    #[test]
    fn defaults_are_written_once_and_never_overwritten() {
        let (dir, s) = store();
        s.ensure_defaults();
        let p = dir.path().join(PROMPTS_DIR);
        for t in [
            Template::Draft,
            Template::Todo,
            Template::DraftChat,
            Template::TodoChat,
            Template::Rewrite,
            Template::Modes,
        ] {
            assert_eq!(
                fs::read_to_string(p.join(t.file())).unwrap(),
                t.default_text()
            );
        }
        fs::write(p.join("draft.txt"), "my own {{memories}}").unwrap();
        s.ensure_defaults();
        assert_eq!(
            fs::read_to_string(p.join("draft.txt")).unwrap(),
            "my own {{memories}}"
        );
    }

    #[test]
    fn the_chat_files_are_added_for_a_user_who_already_has_the_mail_files() {
        let (dir, s) = store();
        let p = dir.path().join(PROMPTS_DIR);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("draft.txt"), "old {{memories}}").unwrap();
        s.ensure_defaults();
        assert_eq!(
            fs::read_to_string(p.join("draft.txt")).unwrap(),
            "old {{memories}}"
        );
        assert_eq!(
            fs::read_to_string(p.join("draft_chat.txt")).unwrap(),
            DEFAULT_DRAFT_CHAT
        );
        assert_eq!(
            fs::read_to_string(p.join("todo_chat.txt")).unwrap(),
            DEFAULT_TODO_CHAT
        );
        // a chat uses its own file, mail keeps its own
        fs::write(p.join("draft_chat.txt"), "Chat: {{memories}}").unwrap();
        assert_eq!(s.draft_chat("N"), "Chat: N");
        assert_eq!(s.draft("N"), "old N");
        // a broken chat file falls back to the chat default, not the mail one
        fs::write(p.join("todo_chat.txt"), "no placeholders").unwrap();
        assert_eq!(s.text(Template::TodoChat), DEFAULT_TODO_CHAT);
        assert!(s
            .todo_chat("", "2026-11-22")
            .contains("today is 2026-11-22"));
    }

    #[test]
    fn an_edited_file_is_used_and_picked_up_when_it_changes() {
        let (dir, s) = store();
        s.ensure_defaults();
        let file = dir.path().join(PROMPTS_DIR).join("draft.txt");
        fs::write(&file, "Short reply. {{memories}}").unwrap();
        assert_eq!(s.draft("NOTE"), "Short reply. NOTE");
        // a changed file with a different stamp is read again (no restart)
        fs::write(&file, "Long reply. {{memories}}").unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(s.draft("NOTE"), "Long reply. NOTE");
    }

    #[test]
    fn a_missing_file_uses_the_default() {
        let (_dir, s) = store();
        assert_eq!(s.text(Template::Draft), DEFAULT_DRAFT);
    }

    #[test]
    fn a_broken_file_falls_back_to_the_default() {
        let (dir, s) = store();
        s.ensure_defaults();
        let p = dir.path().join(PROMPTS_DIR);
        fs::write(p.join("draft.txt"), "   \n").unwrap();
        assert_eq!(s.text(Template::Draft), DEFAULT_DRAFT, "empty");
        fs::write(p.join("todo.txt"), "no placeholders at all").unwrap();
        assert_eq!(s.text(Template::Todo), DEFAULT_TODO, "no {{memories}}");
        fs::write(p.join("rewrite.txt"), "Fix it. {{memories}}").unwrap();
        assert_eq!(
            s.text(Template::Rewrite),
            DEFAULT_REWRITE,
            "no {{instruction}}"
        );
    }

    #[test]
    fn placeholders_are_filled_and_values_are_not_searched_again() {
        let out = fill(
            "a {{memories}} b {{today}} c {{unknown}}",
            &[
                (MEMORIES_PLACEHOLDER, "{{today}}"),
                (TODAY_PLACEHOLDER, "D"),
            ],
        );
        assert_eq!(out, "a {{today}} b D c {{unknown}}");
        let (_dir, s) = store();
        let todo = s.todo("\nNotes\n", "2026-11-22");
        assert!(todo.contains("today is 2026-11-22"));
        assert!(!todo.contains("{{"));
        assert!(!s.rewrite("", "Do X").contains("{{"));
    }

    #[test]
    fn mode_lines_are_read_and_a_missing_one_uses_the_default() {
        let (dir, s) = store();
        s.ensure_defaults();
        fs::write(
            dir.path().join(PROMPTS_DIR).join(MODES_FILE),
            "# mine\nformal: Be very formal.\ngrammar:\n",
        )
        .unwrap();
        assert_eq!(s.instruction(RewriteMode::Formal, ""), "Be very formal.");
        let defaults = parse_modes(DEFAULT_MODES);
        assert_eq!(
            s.instruction(RewriteMode::Grammar, ""),
            defaults["grammar"],
            "an empty line"
        );
        assert_eq!(s.instruction(RewriteMode::Fluent, ""), defaults["fluent"]);
        assert_eq!(s.instruction(RewriteMode::Custom, "  do this "), "do this");
    }

    #[test]
    fn the_defaults_handle_hungarian_and_accents() {
        for (name, text) in [
            ("draft", DEFAULT_DRAFT),
            ("todo", DEFAULT_TODO),
            ("rewrite", DEFAULT_REWRITE),
            ("draft_chat", DEFAULT_DRAFT_CHAT),
            ("todo_chat", DEFAULT_TODO_CHAT),
        ] {
            assert!(text.contains("Hungarian"), "{name}");
            assert!(text.contains("accents"), "{name}");
            assert!(text.contains("á é í ó ö ő ú ü ű"), "{name}");
            assert!(text.contains("köszönöm szépen"), "{name}");
        }
        assert!(DEFAULT_DRAFT.contains("language of the latest message"));
        assert!(DEFAULT_DRAFT_CHAT.contains("language of the latest message"));
        assert!(DEFAULT_DRAFT_CHAT.contains("informal"));
        assert!(DEFAULT_TODO_CHAT.contains("language of the latest message"));
        assert!(DEFAULT_TODO.contains("language of the latest message"));
        assert!(DEFAULT_REWRITE.contains("keep the language of the draft"));
        let modes = parse_modes(DEFAULT_MODES);
        for key in ["grammar", "formal", "fluent", "friendly"] {
            assert!(modes[key].contains("Hungarian accents"), "{key}");
        }
        assert!(DEFAULT_MODES.lines().count() >= 6);
    }
}
