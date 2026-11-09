//! Daemon-performed, append-only, attributed note writes.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use haste_core::policy::{resolve_note_path, NotePathError};

/// Upper bound for one append.
pub const MAX_NOTE_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum AppendError {
    Denied(String),
    TooLarge,
    Io(String),
}

/// Append `text` to `relative` under `root`, preceded by an attribution comment.
/// Existing bytes are never modified. Returns the number of bytes written.
///
/// # Errors
/// [`AppendError::Denied`] for paths outside the allow-list or containing symlinks.
pub fn append_note(
    root: &Path,
    relative: &str,
    agent: &str,
    text: &str,
    ts: i64,
) -> Result<u64, AppendError> {
    if text.len() > MAX_NOTE_BYTES {
        return Err(AppendError::TooLarge);
    }
    let target = resolve_note_path(root, relative).map_err(|e| match e {
        NotePathError::NotAllowed => AppendError::Denied("path outside allow-list".into()),
        NotePathError::Symlink(_) => AppendError::Denied("symlink in path".into()),
        NotePathError::Io(m) => AppendError::Io(m),
    })?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| AppendError::Io(e.to_string()))?;
    }
    let existing_len = fs::symlink_metadata(&target).map(|m| m.len()).unwrap_or(0);
    let needs_newline = if existing_len > 0 {
        let mut f = fs::File::open(&target).map_err(|e| AppendError::Io(e.to_string()))?;
        f.seek(SeekFrom::End(-1))
            .map_err(|e| AppendError::Io(e.to_string()))?;
        let mut last = [0_u8; 1];
        f.read_exact(&mut last)
            .map_err(|e| AppendError::Io(e.to_string()))?;
        last[0] != b'\n'
    } else {
        false
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&target)
        .map_err(|e| AppendError::Io(e.to_string()))?;
    let mut block = String::new();
    if needs_newline {
        block.push('\n');
    }
    block.push_str(&format!(
        "<!-- haste: appended by {agent} at {ts} -->\n{text}\n"
    ));
    file.write_all(block.as_bytes())
        .map_err(|e| AppendError::Io(e.to_string()))?;
    Ok(block.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_is_attributed_and_preserves_existing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("people")).unwrap();
        let file = dir.path().join("people/ana.md");
        fs::write(&file, "# Ana").unwrap();
        append_note(dir.path(), "people/ana.md", "agent-a", "likes tea", 42).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# Ana\n<!-- haste: appended by agent-a at 42 -->\nlikes tea\n"));
    }

    #[test]
    fn creates_new_file_inside_allow_list() {
        let dir = tempfile::tempdir().unwrap();
        append_note(dir.path(), "tasks/new/todo.md", "a", "x", 1).unwrap();
        assert!(dir.path().join("tasks/new/todo.md").exists());
    }

    #[test]
    fn outside_allow_list_and_traversal_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["private/keys.md", "people/../private/keys.md"] {
            assert!(matches!(
                append_note(dir.path(), bad, "a", "x", 1),
                Err(AppendError::Denied(_))
            ));
        }
        assert!(!dir.path().join("private").exists());
    }

    #[test]
    fn symlinked_target_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("tasks")).unwrap();
        fs::write(outside.path().join("t.md"), "secret").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("t.md"),
            dir.path().join("tasks/todo.md"),
        )
        .unwrap();
        assert!(matches!(
            append_note(dir.path(), "tasks/todo.md", "a", "x", 1),
            Err(AppendError::Denied(_))
        ));
        assert_eq!(
            fs::read_to_string(outside.path().join("t.md")).unwrap(),
            "secret"
        );
    }
}
