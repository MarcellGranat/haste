//! The policy gate: a pure function over a capability matrix.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Path prefixes (relative to the notes root) agents may append to.
pub const NOTE_ALLOWED_PREFIXES: [&str; 2] = ["people", "tasks"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    Human,
    Agent,
    Machine,
}

impl PrincipalKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Machine => "machine",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "human" => Some(Self::Human),
            "agent" => Some(Self::Agent),
            "machine" => Some(Self::Machine),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    pub kind: PrincipalKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    Draft,
    Archive,
    Snooze,
    ReminderCreate,
    ReminderComplete,
    NoteAppend,
    Send,
    Approve,
    Reject,
    Manage,
}

impl Action {
    pub const ALL: [Self; 11] = [
        Self::Read,
        Self::Draft,
        Self::Archive,
        Self::Snooze,
        Self::ReminderCreate,
        Self::ReminderComplete,
        Self::NoteAppend,
        Self::Send,
        Self::Approve,
        Self::Reject,
        Self::Manage,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Draft => "draft",
            Self::Archive => "archive",
            Self::Snooze => "snooze",
            Self::ReminderCreate => "reminder.create",
            Self::ReminderComplete => "reminder.complete",
            Self::NoteAppend => "note.append",
            Self::Send => "send",
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Manage => "manage",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Auto,
    Pending,
    Denied,
}

impl Outcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Pending => "pending",
            Self::Denied => "denied",
        }
    }
}

/// What an action is aimed at; only note paths affect the decision today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    None,
    NotePath(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub outcome: Outcome,
    pub reason: &'static str,
}

/// Capability matrix keyed by principal id and action. Missing entries mean `denied`.
#[derive(Debug, Clone, Default)]
pub struct Matrix {
    entries: HashMap<(String, Action), Outcome>,
}

impl Matrix {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the decided default row set for a principal of the given kind.
    pub fn grant_defaults(&mut self, principal: &Principal) {
        for action in Action::ALL {
            let outcome = default_outcome(principal.kind, action);
            self.entries.insert((principal.id.clone(), action), outcome);
        }
    }

    /// Override one entry. `archive` and `snooze` can never be set to `auto`.
    pub fn set(&mut self, principal_id: &str, action: Action, outcome: Outcome) {
        let outcome = if is_never_auto(action) && outcome == Outcome::Auto {
            Outcome::Pending
        } else {
            outcome
        };
        self.entries
            .insert((principal_id.to_owned(), action), outcome);
    }

    #[must_use]
    pub fn get(&self, principal_id: &str, action: Action) -> Option<Outcome> {
        self.entries
            .get(&(principal_id.to_owned(), action))
            .copied()
    }
}

/// Actions that never resolve to `auto` for anyone: each one changes something outside haste (or
/// the user's mailbox state) and always needs a human decision through a proposal.
#[must_use]
pub fn is_never_auto(action: Action) -> bool {
    matches!(
        action,
        Action::Archive | Action::Snooze | Action::ReminderCreate | Action::ReminderComplete
    )
}

/// The decided matrix, per principal kind.
#[must_use]
pub fn default_outcome(kind: PrincipalKind, action: Action) -> Outcome {
    match kind {
        PrincipalKind::Human => Outcome::Auto,
        PrincipalKind::Agent => match action {
            Action::Read | Action::NoteAppend => Outcome::Auto,
            Action::Draft
            | Action::Archive
            | Action::Snooze
            | Action::ReminderCreate
            | Action::ReminderComplete => Outcome::Pending,
            Action::Send | Action::Approve | Action::Reject | Action::Manage => Outcome::Denied,
        },
        PrincipalKind::Machine => match action {
            Action::Read => Outcome::Auto,
            _ => Outcome::Denied,
        },
    }
}

/// Pure gate evaluation. No I/O, no clock.
#[must_use]
pub fn evaluate(
    principal: &Principal,
    action: Action,
    target: &Target,
    matrix: &Matrix,
) -> Decision {
    if principal.kind != PrincipalKind::Human
        && matches!(
            action,
            Action::Send | Action::Approve | Action::Reject | Action::Manage
        )
    {
        return Decision {
            outcome: Outcome::Denied,
            reason: "human_only_action",
        };
    }
    let Some(outcome) = matrix.get(&principal.id, action) else {
        return Decision {
            outcome: Outcome::Denied,
            reason: "no_matrix_entry",
        };
    };
    if is_never_auto(action) && outcome == Outcome::Auto {
        return Decision {
            outcome: Outcome::Pending,
            reason: "never_auto",
        };
    }
    if action == Action::NoteAppend && outcome == Outcome::Auto {
        return match target {
            Target::NotePath(path) if note_path_allowed(path) => Decision {
                outcome: Outcome::Auto,
                reason: "allow_listed_path",
            },
            _ => Decision {
                outcome: Outcome::Denied,
                reason: "path_outside_allow_list",
            },
        };
    }
    Decision {
        outcome,
        reason: "matrix",
    }
}

/// Lexical check: relative, no `..`/`.`, first component allow-listed, at least two components.
#[must_use]
pub fn note_path_allowed(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') {
        return false;
    }
    let parsed = Path::new(path);
    if parsed.is_absolute() {
        return false;
    }
    let mut names: Vec<&str> = Vec::new();
    for component in parsed.components() {
        match component {
            Component::Normal(name) => match name.to_str() {
                Some(text) => names.push(text),
                None => return false,
            },
            _ => return false,
        }
    }
    names.len() >= 2 && NOTE_ALLOWED_PREFIXES.contains(&names[0])
}

#[derive(Debug, PartialEq, Eq)]
pub enum NotePathError {
    NotAllowed,
    Symlink(PathBuf),
    Io(String),
}

/// Resolve a relative note path under `root`, rejecting symlinks at any component.
///
/// # Errors
/// Returns [`NotePathError`] when the path is outside the allow-list, a component is a
/// symlink, or the filesystem cannot be inspected.
pub fn resolve_note_path(root: &Path, relative: &str) -> Result<PathBuf, NotePathError> {
    if !note_path_allowed(relative) {
        return Err(NotePathError::NotAllowed);
    }
    let canonical_root = fs::canonicalize(root).map_err(|e| NotePathError::Io(e.to_string()))?;
    let mut current = canonical_root.clone();
    for component in Path::new(relative).components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(NotePathError::Symlink(current));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(NotePathError::Io(e.to_string())),
        }
    }
    if !current.starts_with(&canonical_root) {
        return Err(NotePathError::NotAllowed);
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(kind: PrincipalKind) -> Principal {
        Principal {
            id: format!("p-{}", kind.as_str()),
            kind,
        }
    }

    fn matrix_for(p: &Principal) -> Matrix {
        let mut m = Matrix::new();
        m.grant_defaults(p);
        m
    }

    fn agent_outcome(action: Action, target: &Target) -> Outcome {
        let p = principal(PrincipalKind::Agent);
        evaluate(&p, action, target, &matrix_for(&p)).outcome
    }

    #[test]
    fn decided_matrix_for_agents() {
        let ok = Target::NotePath("people/ana.md".into());
        let cases = [
            (Action::Read, Target::None, Outcome::Auto),
            (Action::Draft, Target::None, Outcome::Pending),
            (Action::Archive, Target::None, Outcome::Pending),
            (Action::Snooze, Target::None, Outcome::Pending),
            (Action::ReminderCreate, Target::None, Outcome::Pending),
            (Action::NoteAppend, ok, Outcome::Auto),
            (
                Action::NoteAppend,
                Target::NotePath("private/keys.md".into()),
                Outcome::Denied,
            ),
            (Action::Send, Target::None, Outcome::Denied),
            (Action::Approve, Target::None, Outcome::Denied),
            (Action::Reject, Target::None, Outcome::Denied),
            (Action::Manage, Target::None, Outcome::Denied),
        ];
        for (action, target, expected) in cases {
            assert_eq!(agent_outcome(action, &target), expected, "{action:?}");
        }
    }

    #[test]
    fn traversal_and_absolute_paths_are_denied() {
        for bad in [
            "people/../private/keys.md",
            "/people/ana.md",
            "../people/ana.md",
            "people",
            "people//",
            "",
            "./people/ana.md",
        ] {
            assert_eq!(
                agent_outcome(Action::NoteAppend, &Target::NotePath(bad.into())),
                Outcome::Denied,
                "{bad}"
            );
        }
    }

    #[test]
    fn reminder_actions_are_pending_for_agents_denied_for_machines_and_never_auto_for_anyone() {
        let agent = principal(PrincipalKind::Agent);
        let m = matrix_for(&agent);
        for action in [Action::ReminderCreate, Action::ReminderComplete] {
            assert_eq!(
                evaluate(&agent, action, &Target::None, &m).outcome,
                Outcome::Pending,
                "{action:?}"
            );
            let machine = principal(PrincipalKind::Machine);
            let mm = matrix_for(&machine);
            assert_eq!(
                evaluate(&machine, action, &Target::None, &mm).outcome,
                Outcome::Denied,
                "{action:?}"
            );
            // never `auto`, whatever the principal kind and whatever an override says
            for kind in [
                PrincipalKind::Human,
                PrincipalKind::Agent,
                PrincipalKind::Machine,
            ] {
                let p = principal(kind);
                let mut matrix = matrix_for(&p);
                matrix.set(&p.id, action, Outcome::Auto);
                assert_ne!(
                    evaluate(&p, action, &Target::None, &matrix).outcome,
                    Outcome::Auto,
                    "{kind:?} {action:?}"
                );
            }
        }
        assert_eq!(
            Action::parse("reminder.complete"),
            Some(Action::ReminderComplete)
        );
    }

    #[test]
    fn archive_and_snooze_never_resolve_to_auto() {
        for kind in [PrincipalKind::Agent, PrincipalKind::Machine] {
            let p = principal(kind);
            let mut m = matrix_for(&p);
            for action in [Action::Archive, Action::Snooze] {
                m.set(&p.id, action, Outcome::Auto);
                assert_ne!(
                    evaluate(&p, action, &Target::None, &m).outcome,
                    Outcome::Auto
                );
            }
        }
    }

    #[test]
    fn send_is_denied_for_every_non_human_principal() {
        for kind in [PrincipalKind::Agent, PrincipalKind::Machine] {
            let p = principal(kind);
            let mut m = matrix_for(&p);
            m.set(&p.id, Action::Send, Outcome::Auto);
            assert_eq!(
                evaluate(&p, Action::Send, &Target::None, &m).outcome,
                Outcome::Denied
            );
        }
    }

    #[test]
    fn missing_entry_is_denied() {
        let p = principal(PrincipalKind::Agent);
        let d = evaluate(&p, Action::Read, &Target::None, &Matrix::new());
        assert_eq!(d.outcome, Outcome::Denied);
    }

    #[test]
    fn machine_is_read_only_by_default() {
        let p = principal(PrincipalKind::Machine);
        let m = matrix_for(&p);
        assert_eq!(
            evaluate(&p, Action::Read, &Target::None, &m).outcome,
            Outcome::Auto
        );
        assert_eq!(
            evaluate(&p, Action::Draft, &Target::None, &m).outcome,
            Outcome::Denied
        );
    }

    #[test]
    fn symlink_components_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("people")).unwrap();
        fs::create_dir_all(dir.path().join("tasks")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("people/link")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("t.md"),
            dir.path().join("tasks/todo.md"),
        )
        .unwrap();
        assert!(matches!(
            resolve_note_path(dir.path(), "people/link/x.md"),
            Err(NotePathError::Symlink(_))
        ));
        assert!(matches!(
            resolve_note_path(dir.path(), "tasks/todo.md"),
            Err(NotePathError::Symlink(_))
        ));
        assert!(resolve_note_path(dir.path(), "people/new.md").is_ok());
        assert_eq!(
            resolve_note_path(dir.path(), "private/x.md"),
            Err(NotePathError::NotAllowed)
        );
    }
}
