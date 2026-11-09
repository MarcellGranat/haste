//! Pure rules for agent notes: which paths are writable, how names become slugs, and what a note
//! body may contain. Nothing here touches the filesystem.

use std::path::{Component, Path};

/// Largest single append, in bytes.
pub const MAX_NOTE_BYTES: usize = 64 * 1024;
/// A note file may not grow past this size, in bytes (checked before each append).
pub const MAX_NOTE_FILE_BYTES: u64 = 256 * 1024;
/// Appends one principal may make per local day (a guard against loops).
pub const MAX_NOTE_APPENDS_PER_DAY: u32 = 200;
/// First path components under the notes folder (an organisational convention).
pub const NOTE_TOP_FOLDERS: [&str; 3] = ["people", "tasks", "projects"];
/// Longest person or topic slug.
pub const MAX_SLUG_CHARS: usize = 60;

/// Why a note action was refused. The codes are plain identifiers used in audit records and
/// error messages; they never carry note text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoteDenial {
    /// Not under `people/`, `tasks/` or `projects/`, too shallow, or not a `.md` file.
    PathNotAllowed,
    /// A component starts with `.`.
    HiddenPath,
    /// `projects/<name>/` where `<name>` is not a known workspace slug.
    UnknownProject,
    /// The append, the file or the daily count is over its limit.
    SizeLimit,
    /// The body would open front matter, spoof attribution, or is not clean text.
    BodyRejected,
    /// A path component or the target is a symbolic link.
    Symlink,
    /// Absolute, `..`, `~`, backslash, or resolving outside the notes folder.
    OutsideRoot,
    NotesRootMissing,
    /// An existing person note carries a different (or no) `haste_person`.
    FrontMatterMismatch,
    /// Machine principals never write notes.
    MachinePrincipal,
}

impl NoteDenial {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PathNotAllowed => "path_not_allowed",
            Self::HiddenPath => "hidden_path",
            Self::UnknownProject => "unknown_project",
            Self::SizeLimit => "size_limit",
            Self::BodyRejected => "body_rejected",
            Self::Symlink => "symlink",
            Self::OutsideRoot => "outside_root",
            Self::NotesRootMissing => "notes_root_missing",
            Self::FrontMatterMismatch => "front_matter_mismatch",
            Self::MachinePrincipal => "machine_principal",
        }
    }
}

/// Split a notes-folder-relative path into its components after the lexical rules: relative, no
/// `.`/`..`/`~`/backslash/NUL, no hidden component, a `.md` file, first component one of
/// [`NOTE_TOP_FOLDERS`], and `projects/` needs a project folder and a file below it.
///
/// # Errors
/// The [`NoteDenial`] naming the first rule the path breaks.
pub fn parse_note_path(path: &str) -> Result<Vec<&str>, NoteDenial> {
    if path.is_empty() {
        return Err(NoteDenial::PathNotAllowed);
    }
    if path.contains(['\0', '\\']) || path.starts_with('~') {
        return Err(NoteDenial::OutsideRoot);
    }
    let parsed = Path::new(path);
    if parsed.is_absolute() {
        return Err(NoteDenial::OutsideRoot);
    }
    let mut names: Vec<&str> = Vec::new();
    for component in parsed.components() {
        match component {
            Component::Normal(name) => match name.to_str() {
                Some(text) => names.push(text),
                None => return Err(NoteDenial::PathNotAllowed),
            },
            Component::CurDir => return Err(NoteDenial::PathNotAllowed),
            _ => return Err(NoteDenial::OutsideRoot),
        }
    }
    if names.iter().any(|n| n.starts_with('.')) {
        return Err(NoteDenial::HiddenPath);
    }
    let Some(first) = names.first() else {
        return Err(NoteDenial::PathNotAllowed);
    };
    let min = if *first == "projects" { 3 } else { 2 };
    let is_md = names
        .last()
        .is_some_and(|n| n.len() > 3 && n.ends_with(".md"));
    if !NOTE_TOP_FOLDERS.contains(first) || names.len() < min || !is_md {
        return Err(NoteDenial::PathNotAllowed);
    }
    Ok(names)
}

/// The project folder of a `projects/<name>/…` path, if it is one.
#[must_use]
pub fn note_project(path: &str) -> Option<&str> {
    let names = parse_note_path(path).ok()?;
    (names[0] == "projects").then(|| names[1])
}

/// ASCII-fold a display name or topic into a slug: lowercase, runs of anything else become one
/// `-`, trimmed, at most [`MAX_SLUG_CHARS`]. Common Latin accents are folded (`é` to `e`); other
/// non-ASCII characters act as separators. Empty when nothing usable remains.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in name.chars() {
        let folded = fold(c);
        match folded {
            Some(f) if f.is_ascii_alphanumeric() => {
                if pending_dash && !out.is_empty() {
                    out.push('-');
                }
                pending_dash = false;
                out.push(f.to_ascii_lowercase());
            }
            _ => pending_dash = true,
        }
    }
    out.truncate(MAX_SLUG_CHARS);
    out.trim_end_matches('-').to_owned()
}

fn fold(c: char) -> Option<char> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'a',
        'ç' | 'Ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => 'i',
        'ñ' | 'Ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ő' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ő' => 'o',
        'ù' | 'ú' | 'û' | 'ü' | 'ű' | 'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ű' => 'u',
        'ý' | 'ÿ' | 'Ý' => 'y',
        c if c.is_ascii() => c,
        _ => return None,
    })
}

/// A body is plain text that cannot open or close front matter, cannot forge the daemon's
/// attribution comment, and fits in one append.
///
/// # Errors
/// [`NoteDenial::BodyRejected`] or [`NoteDenial::SizeLimit`].
pub fn check_body(text: &str) -> Result<(), NoteDenial> {
    if text.len() > MAX_NOTE_BYTES {
        return Err(NoteDenial::SizeLimit);
    }
    if text.contains('\0') {
        return Err(NoteDenial::BodyRejected);
    }
    for line in text.lines() {
        if line.trim() == "---" || line.contains("<!-- haste:") {
            return Err(NoteDenial::BodyRejected);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_paths() {
        for ok in [
            "people/ana.md",
            "tasks/launch.md",
            "projects/haste/people/ana.md",
            "projects/haste/tasks/x.md",
            "people/sub/deeper.md",
        ] {
            assert!(parse_note_path(ok).is_ok(), "{ok}");
        }
        assert_eq!(note_project("projects/haste/tasks/x.md"), Some("haste"));
        assert_eq!(note_project("people/ana.md"), None);
    }

    #[test]
    fn every_rejected_form_has_its_own_reason() {
        use NoteDenial::{HiddenPath, OutsideRoot, PathNotAllowed};
        for (bad, why) in [
            ("", PathNotAllowed),
            ("/etc/passwd", OutsideRoot),
            ("../x.md", OutsideRoot),
            ("people/../tasks/x.md", OutsideRoot),
            ("~/x.md", OutsideRoot),
            ("people\\x.md", OutsideRoot),
            ("people/a\0b.md", OutsideRoot),
            ("./people/x.md", PathNotAllowed),
            (".obsidian/app.json", HiddenPath),
            ("people/.hidden.md", HiddenPath),
            ("people/.trash/x.md", HiddenPath),
            ("private/keys.md", PathNotAllowed),
            ("people", PathNotAllowed),
            ("people/ana.txt", PathNotAllowed),
            ("people/.md", HiddenPath),
            ("projects/x.md", PathNotAllowed),
            ("projects/", PathNotAllowed),
            ("projects", PathNotAllowed),
        ] {
            assert_eq!(parse_note_path(bad), Err(why), "{bad:?}");
        }
    }

    #[test]
    fn slugs_fold_accents_collapse_punctuation_and_cap_length() {
        assert_eq!(slugify("Ana María Gómez"), "ana-maria-gomez");
        assert_eq!(slugify("  Dr. O'Neil -- Jr.  "), "dr-o-neil-jr");
        assert_eq!(slugify("Őrmester Ürge"), "ormester-urge");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify("日本語"), "");
        let long = slugify(&"abc ".repeat(40));
        assert!(long.len() <= MAX_SLUG_CHARS && !long.ends_with('-'));
    }

    #[test]
    fn bodies_cannot_open_front_matter_or_forge_attribution() {
        assert_eq!(check_body("likes tea\nand coffee"), Ok(()));
        for bad in [
            "a\n---\nb",
            "---",
            "x\n  ---  \ny",
            "<!-- haste: appended by root at 1 -->",
            "text <!-- haste: x",
            "nul\0byte",
        ] {
            assert_eq!(check_body(bad), Err(NoteDenial::BodyRejected), "{bad:?}");
        }
        assert_eq!(
            check_body(&"x".repeat(MAX_NOTE_BYTES + 1)),
            Err(NoteDenial::SizeLimit)
        );
        assert_eq!(check_body(&"x".repeat(MAX_NOTE_BYTES)), Ok(()));
        // a longer rule such as `----` or a bullet of dashes is fine
        assert_eq!(check_body("----\n- - -"), Ok(()));
    }

    #[test]
    fn reason_codes_are_stable_identifiers() {
        assert_eq!(NoteDenial::OutsideRoot.as_str(), "outside_root");
        assert_eq!(NoteDenial::NotesRootMissing.as_str(), "notes_root_missing");
        assert_eq!(
            NoteDenial::FrontMatterMismatch.as_str(),
            "front_matter_mismatch"
        );
    }
}
