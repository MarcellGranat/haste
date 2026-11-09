//! The policy gate: a pure function over a capability matrix.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::notes::{parse_note_path, NoteDenial};

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

/// How an agent's note appends are decided: written directly (`auto`, the default) or queued as
/// a proposal for the human (`approval`). It never affects machine principals, which are denied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteMode {
    #[default]
    Auto,
    Approval,
}

impl NoteMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Approval => "approval",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "approval" => Some(Self::Approval),
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
    CalendarCreate,
    CalendarUpdate,
    NoteAppend,
    Send,
    Approve,
    Reject,
    Manage,
}

impl Action {
    pub const ALL: [Self; 13] = [
        Self::Read,
        Self::Draft,
        Self::Archive,
        Self::Snooze,
        Self::ReminderCreate,
        Self::ReminderComplete,
        Self::CalendarCreate,
        Self::CalendarUpdate,
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
            Self::CalendarCreate => "calendar.create",
            Self::CalendarUpdate => "calendar.update",
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
    note_modes: HashMap<String, NoteMode>,
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

    /// Set a principal's note mode (`auto` by default).
    pub fn set_note_mode(&mut self, principal_id: &str, mode: NoteMode) {
        self.note_modes.insert(principal_id.to_owned(), mode);
    }

    #[must_use]
    pub fn note_mode(&self, principal_id: &str) -> NoteMode {
        self.note_modes
            .get(principal_id)
            .copied()
            .unwrap_or_default()
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
        Action::Archive
            | Action::Snooze
            | Action::ReminderCreate
            | Action::ReminderComplete
            | Action::CalendarCreate
            | Action::CalendarUpdate
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
            | Action::ReminderComplete
            | Action::CalendarCreate
            | Action::CalendarUpdate => Outcome::Pending,
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
    // Machine principals never write notes, whatever the matrix says: this rule is outside the
    // matrix so no configuration can grant it.
    if action == Action::NoteAppend && principal.kind == PrincipalKind::Machine {
        return Decision {
            outcome: Outcome::Denied,
            reason: NoteDenial::MachinePrincipal.as_str(),
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
            Target::NotePath(path) => match parse_note_path(path) {
                Ok(_) if matrix.note_mode(&principal.id) == NoteMode::Approval => Decision {
                    outcome: Outcome::Pending,
                    reason: "note_approval_mode",
                },
                Ok(_) => Decision {
                    outcome: Outcome::Auto,
                    reason: "allow_listed_path",
                },
                Err(denial) => Decision {
                    outcome: Outcome::Denied,
                    reason: denial.as_str(),
                },
            },
            Target::None => Decision {
                outcome: Outcome::Denied,
                reason: NoteDenial::PathNotAllowed.as_str(),
            },
        };
    }
    Decision {
        outcome,
        reason: "matrix",
    }
}

/// Lexical check of a notes-folder-relative path; see [`parse_note_path`] for the rules.
#[must_use]
pub fn note_path_allowed(path: &str) -> bool {
    parse_note_path(path).is_ok()
}

#[derive(Debug, PartialEq, Eq)]
pub enum NotePathError {
    Denied(NoteDenial),
    Io(String),
}

/// Resolve a relative note path under `root`: the lexical rules first, then no symlink at any
/// component, and the result must stay inside the canonical root.
///
/// # Errors
/// [`NotePathError::Denied`] with the reason, or `Io` when the filesystem cannot be inspected
/// (including a missing root).
pub fn resolve_note_path(root: &Path, relative: &str) -> Result<PathBuf, NotePathError> {
    parse_note_path(relative).map_err(NotePathError::Denied)?;
    let canonical_root = fs::canonicalize(root).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => NotePathError::Denied(NoteDenial::NotesRootMissing),
        _ => NotePathError::Io(e.to_string()),
    })?;
    let mut current = canonical_root.clone();
    for component in Path::new(relative).components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(NotePathError::Denied(NoteDenial::Symlink));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(NotePathError::Io(e.to_string())),
        }
    }
    if !current.starts_with(&canonical_root) {
        return Err(NotePathError::Denied(NoteDenial::OutsideRoot));
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
    fn calendar_actions_are_pending_for_agents_never_auto_and_delete_is_undefined() {
        let agent = principal(PrincipalKind::Agent);
        let m = matrix_for(&agent);
        for action in [Action::CalendarCreate, Action::CalendarUpdate] {
            assert_eq!(
                evaluate(&agent, action, &Target::None, &m).outcome,
                Outcome::Pending,
                "{action:?}"
            );
            let machine = principal(PrincipalKind::Machine);
            assert_eq!(
                evaluate(&machine, action, &Target::None, &matrix_for(&machine)).outcome,
                Outcome::Denied,
                "machines are read-only"
            );
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
        // there is no delete action to grant, request or approve
        assert_eq!(Action::parse("calendar.delete"), None);
        assert_eq!(
            Action::parse("calendar.create"),
            Some(Action::CalendarCreate)
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
    fn note_mode_decides_every_principal_kind_and_path_class() {
        let good = Target::NotePath("people/ana.md".into());
        let hidden = Target::NotePath(".obsidian/app.json".into());
        let outside = Target::NotePath("../x.md".into());
        for (kind, mode, target, expect) in [
            (PrincipalKind::Agent, NoteMode::Auto, &good, Outcome::Auto),
            (
                PrincipalKind::Agent,
                NoteMode::Approval,
                &good,
                Outcome::Pending,
            ),
            (
                PrincipalKind::Agent,
                NoteMode::Auto,
                &hidden,
                Outcome::Denied,
            ),
            (
                PrincipalKind::Agent,
                NoteMode::Approval,
                &hidden,
                Outcome::Denied,
            ),
            (
                PrincipalKind::Agent,
                NoteMode::Auto,
                &outside,
                Outcome::Denied,
            ),
            (
                PrincipalKind::Agent,
                NoteMode::Approval,
                &outside,
                Outcome::Denied,
            ),
            (
                PrincipalKind::Machine,
                NoteMode::Auto,
                &good,
                Outcome::Denied,
            ),
            (
                PrincipalKind::Machine,
                NoteMode::Approval,
                &good,
                Outcome::Denied,
            ),
            (PrincipalKind::Human, NoteMode::Auto, &good, Outcome::Auto),
        ] {
            let p = principal(kind);
            let mut m = matrix_for(&p);
            m.set_note_mode(&p.id, mode);
            assert_eq!(
                evaluate(&p, Action::NoteAppend, target, &m).outcome,
                expect,
                "{kind:?} {mode:?} {target:?}"
            );
        }
        assert_eq!(NoteMode::default(), NoteMode::Auto, "the default is auto");
        assert_eq!(NoteMode::parse("approval"), Some(NoteMode::Approval));
        assert_eq!(NoteMode::parse("sometimes"), None);
    }

    #[test]
    fn a_machine_principal_can_never_append_notes_even_with_a_matrix_grant() {
        let p = principal(PrincipalKind::Machine);
        let mut m = matrix_for(&p);
        m.set(&p.id, Action::NoteAppend, Outcome::Auto);
        let d = evaluate(
            &p,
            Action::NoteAppend,
            &Target::NotePath("people/ana.md".into()),
            &m,
        );
        assert_eq!(
            (d.outcome, d.reason),
            (Outcome::Denied, "machine_principal")
        );
    }

    #[test]
    fn denied_note_paths_name_their_reason() {
        let p = principal(PrincipalKind::Agent);
        let m = matrix_for(&p);
        for (path, reason) in [
            ("../x.md", "outside_root"),
            (".obsidian/app.json", "hidden_path"),
            ("private/x.md", "path_not_allowed"),
        ] {
            let d = evaluate(&p, Action::NoteAppend, &Target::NotePath(path.into()), &m);
            assert_eq!((d.outcome, d.reason), (Outcome::Denied, reason), "{path}");
        }
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
            Err(NotePathError::Denied(NoteDenial::Symlink))
        ));
        assert!(matches!(
            resolve_note_path(dir.path(), "tasks/todo.md"),
            Err(NotePathError::Denied(NoteDenial::Symlink))
        ));
        assert!(resolve_note_path(dir.path(), "people/new.md").is_ok());
        assert_eq!(
            resolve_note_path(dir.path(), "private/x.md"),
            Err(NotePathError::Denied(NoteDenial::PathNotAllowed))
        );
        assert_eq!(
            resolve_note_path(dir.path(), "../x.md"),
            Err(NotePathError::Denied(NoteDenial::OutsideRoot))
        );
        assert_eq!(
            resolve_note_path(&dir.path().join("missing-root"), "people/x.md"),
            Err(NotePathError::Denied(NoteDenial::NotesRootMissing))
        );
    }
}
