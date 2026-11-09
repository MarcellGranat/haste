## Why

Agents working in herdr panes already see people, tasks and emails that matter to the user, but
they have nowhere durable to keep what they learn. The core already has an allow-listed,
append-only `note.append` action that writes under a notes root. This change points that root at
an Obsidian vault so agents can leave dated notes about people, and identifier-only back-references
to tasks, emails and events, where the user already reads and organises notes. Obsidian was
explicitly out of scope for `reminders-and-links`; this change takes it on.

## What Changes

- The notes root becomes a configured **Obsidian vault folder**. Paths are vault-relative. Any path
  component starting with `.` (including `.obsidian/`) is never writable by any principal.
- **Person notes**: an agent can append a dated entry to `people/<slug>.md`, a note linked to the
  haste `Person` record by its id. Front matter is written once, when the file is created. Aliases
  from known identifiers are an opt-in. Email addresses are never written unless the human opts in.
- **Task and email back-references**: notes under `tasks/` (and per-project folders) hold only
  identifier lines such as `haste:thread/<id>`, `haste:reminder/<id>` and `haste:event/<id>`. The
  daemon composes these lines from typed references. No subject, body or attendee address is ever
  written by this path.
- **Append-only for agents**: no edit, delete, overwrite or front-matter rewrite. Size limits apply
  per append and per file.
- **Human review**: a human-only way to read what was written (`haste note show`, `haste note log`)
  and an optional approval mode in which each agent note becomes a `NoteAppend` proposal that the
  human approves before the daemon writes it.
- **Machine principals never write notes.** A project tag taken from a herdr workspace label places
  notes under a per-project folder, but only when the label is a known project label.
- Tests use fakes and temporary directories only. Audit records carry counts, paths and byte lengths,
  never note text.
- A small human spike group runs first (where the vault lives on this Mac, whether iCloud or Obsidian
  Sync touches it, and the conflict risk of appending to a synced file). A final manual smoke group
  closes the change. Agents on remote machines are out of scope: the hub writes the vault.

**BREAKING**: `people/**` and `tasks/**` remain the only agent-writable prefixes, but they now resolve
under the configured vault folder rather than an arbitrary notes root. Existing `HASTE_NOTES_ROOT`
users must point it at the vault.

## Capabilities

### New Capabilities
- `obsidian-vault`: vault root configuration, vault-relative path rules, hidden-path and symlink rules, size limits, and the human-readable record of what was appended.
- `person-notes`: one note per `Person` record, front matter written once, dated entries, opt-in aliases and the rule that addresses are not written by default.
- `work-reference-notes`: identifier-only back-reference notes for threads, reminders and events, with no free text from agents.
- `note-review`: human-only read of written notes, the audit view without text, and the approval mode that turns agent note appends into proposals.

### Modified Capabilities
- `policy-gate`: `note.append` resolves against the vault allow-list and per-project folders, machine principals are always denied, and approval mode resolves agent appends to `pending`.
- `proposals`: adds the `NoteAppend` payload, its execution on approval through the daemon's note executor, and its expiry rules.

## Impact

- Code: `crates/haste-core` (policy path predicate, note reference and person-note types), `crates/hasted` (`notes.rs` executor, `dispatch.rs` note handling, config loading, project label check), `crates/haste-proto` (typed note requests and the human read request), `crates/haste` (human-only `note show` and `note log` verbs), `crates/haste-tui` (optional approval rendering for `NoteAppend`), `crates/haste-store` (proposal payload variant, no new tables expected).
- Systems: macOS filesystem; an Obsidian vault folder, possibly synced by iCloud Drive or Obsidian Sync (see the spikes).
- Dependencies: none new expected. Front matter is written as plain text, without a YAML crate.
- Depends on: `reminders-and-links` (archived; reminder ids exist) and, for `haste:event/<id>`, the `google-calendar` change, which is not yet archived. Event back-references are gated on it.
- Audit: unchanged in format. `note.append` records keep path and byte length only.
