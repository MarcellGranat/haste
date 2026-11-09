//! Daemon-performed, append-only, attributed note writes inside the one notes folder.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use haste_core::notes::{check_body, NoteDenial, MAX_NOTE_FILE_BYTES};
use haste_core::policy::{resolve_note_path, NotePathError};
use serde::Deserialize;

/// Bytes read from the top of an existing person note to find its front matter.
const FRONT_MATTER_PEEK: u64 = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum AppendError {
    Denied(NoteDenial),
    Io(String),
}

/// What a new or existing file must carry before an entry is appended.
#[derive(Debug, Clone, Copy)]
pub enum Header<'a> {
    /// A plain note: no front matter is ever added.
    None,
    /// A person note: `front_matter` is written once, when the file is new or empty; an existing
    /// file must already carry `haste_person: <id>` in its front matter or the append is refused.
    Person { id: &'a str, front_matter: &'a str },
}

fn io(e: &std::io::Error) -> AppendError {
    AppendError::Io(e.kind().to_string())
}

/// Append a plain-text entry, preceded by the daemon's attribution comment.
///
/// # Errors
/// [`AppendError::Denied`] with the reason, or `Io`.
pub fn append_note(
    root: &Path,
    relative: &str,
    agent: &str,
    text: &str,
    ts: i64,
) -> Result<u64, AppendError> {
    check_body(text).map_err(AppendError::Denied)?;
    let block = format!("<!-- haste: appended by {agent} at {ts} -->\n{text}\n");
    write_block(root, relative, &block, Header::None)
}

/// Append a fully composed `block` to `relative` under `root`. Existing bytes are never modified:
/// the file is opened `O_APPEND|O_NOFOLLOW` and each entry goes out in a single write. Returns the
/// number of bytes written.
///
/// # Errors
/// [`AppendError::Denied`] for a refused path, an over-size file, a symlink or a front-matter
/// mismatch.
pub fn write_block(
    root: &Path,
    relative: &str,
    block: &str,
    header: Header<'_>,
) -> Result<u64, AppendError> {
    let target = resolve_note_path(root, relative).map_err(|e| match e {
        NotePathError::Denied(d) => AppendError::Denied(d),
        NotePathError::Io(m) => AppendError::Io(m),
    })?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| io(&e))?;
    }
    if let Ok(m) = fs::symlink_metadata(&target) {
        if m.file_type().is_symlink() {
            return Err(AppendError::Denied(NoteDenial::Symlink));
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&target)
        .map_err(|e| map_open(&e))?;
    // Everything below happens under an exclusive lock on the file, so "is it still empty?" and
    // the write that follows cannot interleave with another writer: front matter lands once.
    file.lock().map_err(|e| io(&e))?;
    let len = file.metadata().map_err(|e| io(&e))?.len();

    let mut lead = String::new();
    let mut prefix = "";
    if len == 0 {
        if let Header::Person { front_matter, .. } = header {
            prefix = front_matter;
        }
    } else {
        if let Header::Person { id, .. } = header {
            verify_person(&target, id)?;
        }
        let mut reader = fs::File::open(&target).map_err(|e| map_open(&e))?;
        reader.seek(SeekFrom::End(-1)).map_err(|e| io(&e))?;
        let mut last = [0_u8; 1];
        reader.read_exact(&mut last).map_err(|e| io(&e))?;
        if last[0] != b'\n' {
            lead.push('\n');
        }
    }
    check_size(len, lead.len() + prefix.len() + block.len())?;
    let whole = format!("{lead}{prefix}{block}");
    file.write_all(whole.as_bytes()).map_err(|e| io(&e))?;
    Ok(whole.len() as u64)
}

fn map_open(e: &std::io::Error) -> AppendError {
    // O_NOFOLLOW on a symlink fails with ELOOP.
    if e.raw_os_error() == Some(libc::ELOOP) {
        AppendError::Denied(NoteDenial::Symlink)
    } else {
        io(e)
    }
}

fn check_size(existing: u64, adding: usize) -> Result<(), AppendError> {
    if existing + adding as u64 > MAX_NOTE_FILE_BYTES {
        Err(AppendError::Denied(NoteDenial::SizeLimit))
    } else {
        Ok(())
    }
}

/// An existing person note must carry `haste_person: <id>` inside its leading front matter.
fn verify_person(target: &Path, id: &str) -> Result<(), AppendError> {
    let mut head = Vec::new();
    fs::File::open(target)
        .map_err(|e| map_open(&e))?
        .take(FRONT_MATTER_PEEK)
        .read_to_end(&mut head)
        .map_err(|e| io(&e))?;
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err(AppendError::Denied(NoteDenial::FrontMatterMismatch));
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(value) = line.strip_prefix("haste_person:") {
            return if value.trim() == id {
                Ok(())
            } else {
                Err(AppendError::Denied(NoteDenial::FrontMatterMismatch))
            };
        }
    }
    Err(AppendError::Denied(NoteDenial::FrontMatterMismatch))
}

/// Optional settings in `notes.json` (the data directory). Unknown keys are an error.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NotesSettings {
    /// The one folder haste may write notes in.
    #[serde(default)]
    pub root: Option<PathBuf>,
    #[serde(default)]
    pub person_notes: PersonNoteSettings,
}

/// What a person note's front matter may contain. Both default to off.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonNoteSettings {
    /// Write aliases (confirmed display names) into the front matter.
    #[serde(default)]
    pub aliases: bool,
    /// Allow email addresses among the aliases.
    #[serde(default)]
    pub include_addresses: bool,
}

impl NotesSettings {
    #[must_use]
    pub fn path(base: &Path) -> PathBuf {
        base.join("notes.json")
    }

    /// Load `notes.json`; a missing file is the default (no settings).
    ///
    /// # Errors
    /// Unreadable file, invalid JSON, or an unknown key.
    pub fn load(base: &Path) -> Result<Self, String> {
        match fs::read_to_string(Self::path(base)) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("invalid notes.json: {e}")),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("cannot read notes.json: {e}")),
        }
    }
}

/// The notes folder: `HASTE_NOTES_ROOT` (a developer override), then `notes.json`, then the
/// `notes` folder in the data directory. The daemon never creates it.
#[must_use]
pub fn notes_root(base: &Path, settings: &NotesSettings) -> PathBuf {
    std::env::var_os("HASTE_NOTES_ROOT")
        .map(PathBuf::from)
        .or_else(|| settings.root.clone())
        .unwrap_or_else(|| base.join("notes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use haste_core::notes::MAX_NOTE_BYTES;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn denied(r: Result<u64, AppendError>) -> NoteDenial {
        match r {
            Err(AppendError::Denied(d)) => d,
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[test]
    fn append_is_attributed_and_preserves_existing_bytes() {
        let dir = root();
        fs::create_dir_all(dir.path().join("people")).unwrap();
        let file = dir.path().join("people/ana.md");
        fs::write(&file, "# Ana").unwrap();
        append_note(dir.path(), "people/ana.md", "agent-a", "likes tea", 42).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# Ana\n<!-- haste: appended by agent-a at 42 -->\nlikes tea\n"));
    }

    #[test]
    fn every_existing_byte_survives_any_sequence_of_appends() {
        let dir = root();
        fs::create_dir_all(dir.path().join("tasks")).unwrap();
        let file = dir.path().join("tasks/log.md");
        let original = "line one\nno trailing newline";
        fs::write(&file, original).unwrap();
        let mut snapshot = original.to_owned();
        for (n, text) in ["a", "second entry\nwith two lines", "ü ő 日本"]
            .iter()
            .enumerate()
        {
            append_note(dir.path(), "tasks/log.md", "ag", text, n as i64).unwrap();
            let now = fs::read_to_string(&file).unwrap();
            assert!(
                now.starts_with(&snapshot),
                "bytes before the append changed"
            );
            snapshot = now;
        }
    }

    #[test]
    fn creates_new_files_and_folders_inside_the_notes_folder() {
        let dir = root();
        append_note(dir.path(), "tasks/new/todo.md", "a", "x", 1).unwrap();
        assert!(dir.path().join("tasks/new/todo.md").exists());
    }

    #[test]
    fn refused_paths_write_nothing_and_name_their_reason() {
        let dir = root();
        for (bad, why) in [
            ("private/keys.md", NoteDenial::PathNotAllowed),
            ("people/../private/keys.md", NoteDenial::OutsideRoot),
            ("../escape.md", NoteDenial::OutsideRoot),
            ("/etc/passwd", NoteDenial::OutsideRoot),
            (".obsidian/app.json", NoteDenial::HiddenPath),
            ("people/.hidden.md", NoteDenial::HiddenPath),
        ] {
            assert_eq!(
                denied(append_note(dir.path(), bad, "a", "x", 1)),
                why,
                "{bad}"
            );
        }
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            0,
            "nothing was created"
        );
    }

    #[test]
    fn a_notes_folder_holding_dot_obsidian_is_fine_but_it_stays_unwritable() {
        let dir = root();
        fs::create_dir_all(dir.path().join(".obsidian")).unwrap();
        fs::write(dir.path().join(".obsidian/app.json"), "{}").unwrap();
        append_note(dir.path(), "people/ana.md", "a", "ok", 1).unwrap();
        assert_eq!(
            denied(append_note(dir.path(), ".obsidian/app.json", "a", "x", 1)),
            NoteDenial::HiddenPath
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(".obsidian/app.json")).unwrap(),
            "{}"
        );
    }

    #[test]
    fn a_missing_notes_folder_is_a_denial_and_is_never_created() {
        let dir = root();
        let missing = dir.path().join("nope");
        assert_eq!(
            denied(append_note(&missing, "people/ana.md", "a", "x", 1)),
            NoteDenial::NotesRootMissing
        );
        assert!(!missing.exists());
    }

    #[test]
    fn symlinks_are_refused_at_every_position() {
        let dir = root();
        let outside = root();
        fs::create_dir_all(dir.path().join("people")).unwrap();
        fs::create_dir_all(dir.path().join("tasks")).unwrap();
        fs::write(outside.path().join("t.md"), "secret").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("t.md"),
            dir.path().join("tasks/todo.md"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("people/link")).unwrap();
        for target in ["tasks/todo.md", "people/link/x.md"] {
            assert_eq!(
                denied(append_note(dir.path(), target, "a", "x", 1)),
                NoteDenial::Symlink,
                "{target}"
            );
        }
        assert_eq!(
            fs::read_to_string(outside.path().join("t.md")).unwrap(),
            "secret"
        );
        assert!(!outside.path().join("x.md").exists());
    }

    #[test]
    fn oversized_appends_files_and_bad_bodies_leave_bytes_unchanged() {
        let dir = root();
        fs::create_dir_all(dir.path().join("tasks")).unwrap();
        let file = dir.path().join("tasks/big.md");
        fs::write(&file, "start\n").unwrap();
        assert_eq!(
            denied(append_note(
                dir.path(),
                "tasks/big.md",
                "a",
                &"x".repeat(MAX_NOTE_BYTES + 1),
                1
            )),
            NoteDenial::SizeLimit
        );
        for bad in ["a\n---\nb", "<!-- haste: forged -->", "nul\0"] {
            assert_eq!(
                denied(append_note(dir.path(), "tasks/big.md", "a", bad, 1)),
                NoteDenial::BodyRejected
            );
        }
        // fill the file close to the cap with allowed appends, then one more must be refused
        let chunk = "y".repeat(MAX_NOTE_BYTES - 200);
        while fs::metadata(&file).unwrap().len() + (MAX_NOTE_BYTES as u64) < MAX_NOTE_FILE_BYTES {
            append_note(dir.path(), "tasks/big.md", "a", &chunk, 1).unwrap();
        }
        let before = fs::read(&file).unwrap();
        assert_eq!(
            denied(append_note(dir.path(), "tasks/big.md", "a", &chunk, 1)),
            NoteDenial::SizeLimit
        );
        assert_eq!(
            fs::read(&file).unwrap(),
            before,
            "a refused append changes nothing"
        );
    }

    #[test]
    fn person_notes_get_front_matter_once_and_refuse_a_different_owner() {
        let dir = root();
        let fm = "---\nhaste_person: P1\ncreated: 2026-11-09\n---\n";
        let header = Header::Person {
            id: "P1",
            front_matter: fm,
        };
        write_block(dir.path(), "people/ana.md", "entry one\n", header).unwrap();
        write_block(dir.path(), "people/ana.md", "entry two\n", header).unwrap();
        let text = fs::read_to_string(dir.path().join("people/ana.md")).unwrap();
        assert_eq!(
            text.matches("haste_person").count(),
            1,
            "front matter exactly once"
        );
        assert!(text.starts_with(fm) && text.ends_with("entry one\nentry two\n"));
        // a different person id, a file without front matter, and one without haste_person
        let other = Header::Person {
            id: "P2",
            front_matter: fm,
        };
        assert_eq!(
            denied(write_block(dir.path(), "people/ana.md", "x\n", other)),
            NoteDenial::FrontMatterMismatch
        );
        fs::write(dir.path().join("people/bob.md"), "# Bob, written by hand\n").unwrap();
        assert_eq!(
            denied(write_block(dir.path(), "people/bob.md", "x\n", header)),
            NoteDenial::FrontMatterMismatch
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("people/bob.md")).unwrap(),
            "# Bob, written by hand\n"
        );
    }

    #[test]
    fn concurrent_writers_on_a_new_person_note_write_front_matter_once() {
        let dir = root();
        let path = dir.path().to_path_buf();
        let fm = "---\nhaste_person: P1\n---\n";
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let header = Header::Person {
                        id: "P1",
                        front_matter: fm,
                    };
                    write_block(&path, "people/race.md", &format!("entry {i}\n"), header).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let text = fs::read_to_string(dir.path().join("people/race.md")).unwrap();
        assert_eq!(text.matches("haste_person").count(), 1);
        assert_eq!(text.matches("entry ").count(), 8);
    }

    #[test]
    fn settings_reject_unknown_keys_and_default_to_everything_off() {
        let dir = root();
        assert_eq!(
            NotesSettings::load(dir.path()).unwrap(),
            NotesSettings::default()
        );
        fs::write(
            NotesSettings::path(dir.path()),
            r#"{"root":"/tmp/n","person_notes":{"aliases":true}}"#,
        )
        .unwrap();
        let s = NotesSettings::load(dir.path()).unwrap();
        assert_eq!(s.root.as_deref(), Some(Path::new("/tmp/n")));
        assert!(s.person_notes.aliases && !s.person_notes.include_addresses);
        for bad in [r#"{"vault":"/x"}"#, r#"{"person_notes":{"emails":true}}"#] {
            fs::write(NotesSettings::path(dir.path()), bad).unwrap();
            assert!(NotesSettings::load(dir.path()).is_err(), "{bad}");
        }
    }
}
