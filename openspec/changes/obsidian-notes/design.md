## Context

The daemon already performs `note.append` itself (`crates/hasted/src/notes.rs`). It resolves the
path against `NOTE_ALLOWED_PREFIXES = ["people", "tasks"]` under the notes root (`HASTE_NOTES_ROOT`),
rejects symlink components, opens with `O_APPEND|O_NOFOLLOW`, writes an attribution comment and the
text, and caps one append at `MAX_NOTE_BYTES` (64 KiB). The audit records `bytes=<n>` and the path,
never the text. `NoteAppend` carries `path`, `text` and `idempotency_key`; the dispatcher keeps the
key in memory. Herdr project labels are already validated against the store in dispatch.

What is missing: a user-chosen single notes folder with containment guarantees, person-linked notes, identifier-only back-references, a human
way to read what was written, an approval mode, and rules for a synced notes folder. Constraints: the
daemon is the only writer, agents never edit or delete, machine principals never write, and tests
use fakes and temporary directories only.

## Goals / Non-Goals

**Goals**
- haste has exactly one user-chosen notes folder (the notes root, possibly inside an Obsidian vault); agents see notes-root-relative paths only and nothing outside the folder is written or read.
- Person notes and back-reference notes are produced by the daemon from typed inputs, so agent text
  cannot inject front matter, identifiers of other kinds, addresses or email bodies.
- Every write stays append-only and audited by counts and paths.
- The human can read every note the agents wrote and can require approval before a write.

**Non-Goals**
- Agents on remote herdr machines writing into the notes folder (hub only).
- Editing, deleting, renaming or moving notes, for agents or for the daemon.
- Rendering Obsidian features (Dataview, templates, plugins). Notes are plain Markdown.
- Full-text search of the notes folder from haste.
- Any vault-wide scope: no whole-vault allow-list, no need for the vault root, no requirement that `.obsidian/` exists, no reading of other vault content.
- Syncing the notes folder itself. iCloud or Obsidian Sync stays the user's responsibility; the spikes decide what the change must do about it.

## Decisions

### D1. One user-specified notes folder is the notes root
The notes root is named by a `notes.json` file in the data directory (`{"root": "/path/to/folder"}`,
mode `0600`, no secrets), with `HASTE_NOTES_ROOT` kept as an override. Decision (the human confirmed the
daemon runs under launchd): a launchd job does not inherit a shell's environment, so the settings file
is the primary source and the environment variable is only a developer override. It names exactly one
folder chosen by the user: the notes root. It may be a
subfolder inside an Obsidian vault or any other directory. Rules:
- Every agent write resolves inside this folder. The rest of the vault (if any) is never written and
  never read by haste, and haste never needs the vault root.
- The root must already exist and be a directory; the daemon never creates or modifies the root
  itself. If it is missing, note actions are denied with `notes_root_missing` and the connector status
  is unaffected.
- No `.obsidian/` is required, and no check for it is made. If the configured folder equals, or is
  above, a path that contains `.obsidian`, the configuration is accepted, but haste still writes only
  inside the folder and the hidden-path rule (D2) keeps `.obsidian/` unwritable. *Q1 (is `.obsidian/`
  required) is resolved: removed, not needed.*
- Paths in requests are notes-root-relative: no leading `/`, no `..`, no backslash, no NUL, no `~`.
  Canonicalization happens against the canonical root, and the canonical target must be inside it
  (`outside_root` otherwise), including when a symlink points out.

### D2. Path predicate
A target is writable only if all of these hold:
0. It resolves inside the notes root (containment, checked on the canonical path).
1. Its first component is `people`, `tasks`, or `projects` (an organisational convention inside the notes folder).
2. For `projects/`, its second component is a project slug that equals the slug of a known herdr
   workspace label (D7). A bare `projects/` file is refused.
3. No component starts with `.`. This keeps `.obsidian/`, `.trash/` and `.git/` out if they ever appear inside the notes folder.
4. Each component is a normal name, and the final component ends with `.md`.
5. No component is a symlink (existing check), and the target is not a symlink (`O_NOFOLLOW`).

`resolve_note_path` keeps its shape; the predicate gains rules 0 and 2 to 4.

### D3. Append-only file shape
The executor stays the single writer. It never truncates, renames, or rewrites front matter. A new
file is created by an append, and only then does it receive the front matter (D4). Existing bytes
are never modified. The existing attribution comment stays, so every entry records the agent and
timestamp.

The daemon, not the agent, composes the block. The agent supplies only typed inputs (D4, D5), or,
for the generic `note.append` path, plain text that must pass the body check:
- no line equal to `---` (would open or close front matter);
- no line containing `<!-- haste:` (would spoof attribution);
- no NUL bytes and valid UTF-8;
- at most the per-append limit (D8).

### D4. Person notes
A person note is `people/<slug>.md`. The slug is the ASCII-folded, lowercase display name with runs
of non-alphanumerics replaced by `-`, trimmed, and at most 60 characters. The `Person` record id is
the link, not the slug, so renaming the person later does not break it.

On creation the daemon writes front matter once:

```
---
haste_person: <person id>
created: <YYYY-MM-DD>
aliases: [<opt-in only>]
---
```

- `aliases` is written only when the human has opted in (`person_notes.aliases = true`), and then
  only from display names and identifiers the human has confirmed are not addresses. Email
  addresses are never written unless `person_notes.include_addresses = true`. Default is off for both.
- If the file exists with a different or missing `haste_person`, the append is refused. The daemon
  never adds or changes front matter on an existing file.
- Each entry is a dated heading, `### <YYYY-MM-DD HH:MM>`, followed by the attribution comment and
  the agent text (D3 body check). Times are the daemon's clock, in the local zone.

*Open question Q2: slug collisions between two people with the same name.* Options: a numeric
suffix, or refusing and asking the human to pick the slug. Not decided here.

### D5. Work back-reference notes
Task and email notes contain only identifier lines. The request is typed:
`NoteReference { kind: thread | reminder | event, id }`. The daemon validates `id` against the
identifier charset used by the store (ULID or external id, length-bounded and ASCII) and checks the
object exists. It then appends `- haste:<kind>/<id>` under a dated heading. Agents cannot pass text
through this path.

Where they go: `tasks/<slug>.md` for a task-like note, or `projects/<project>/tasks/<slug>.md` when
a project applies. The slug is the human-supplied topic label, not the email subject.

`haste:event/<id>` is accepted only after the `google-calendar` change is archived, because the
event model comes from it. Until then the kind is refused as unknown. *Open question Q3: whether
task notes may ever carry free text from agents.* The current design says no.

### D6. Approval mode
Approval is a per-principal setting, `note_mode: auto | approval`. Decision (Q4, human): the default
is `auto`: notes land directly, because they are append-only inside one folder the human can see.
`approval` stays available per principal and is off by default. It never applies to machine
principals (D7), which are always denied.

- `auto`: the gate resolves to `auto`, the executor writes, the audit records `written`.
- `approval`: the gate resolves to `pending`, a `NoteAppend` proposal is created with the path and
  text in its payload, and nothing is written. Approving it (human only) runs the same executor under
  the original principal's identity. Rejecting or expiring it writes nothing.

The proposal payload holds the text, which is acceptable because proposals already hold drafts. The
audit still records only the proposal id, path and byte length. Expiry uses the existing 7-day TTL.
Supersession applies to the same principal, path and thread only if both match; otherwise each
proposal stands alone.

*Q4 resolved: the default mode is `auto` for every agent. Still open: whether the `NoteReference` path
(D5) should follow the principal's mode (assumed: yes, it does).*

### D7. Machine principals and project tags
- The gate denies `note.append` for every herdr machine principal, regardless of matrix content.
  This is a new explicit rule, not a matrix entry, so it cannot be granted by config.
- An agent's `hints.workspace` (a routing hint, not identity) selects the project folder only if it
  matches a known workspace label. The slug is computed from the label by the same function as D4.
  An unknown label means the write is refused, not redirected to `people/` or `tasks/`.
- Project folders hold the same note kinds (person, reference) as the top level.

*Open question Q5: should project notes for people live under the project folder or in the shared
`people/` folder?* Not decided here.

### D8. Size limits
- Per append: the existing 64 KiB ceiling stays.
- Per file: a cap is checked before writing, from the file size metadata. The default is to be
  chosen after the spikes. *Open question Q6.*
- Per principal per day: a count cap of appends to protect against loops. Not decided. *Open question Q7.*
- Over-limit requests return `Validation` and write nothing; the audit records `denied` with a reason
  code and no text.

### D9. Human read surfaces
- `haste note show <notes-root-relative path>` (human build only) prints a note as the daemon reads it.
  The daemon checks the human principal and the path predicate (D2); hidden paths are refused.
- `haste note log [--since <ts>]` lists `note.append` audit rows as path, byte length, agent and
  time. It never shows text, because the audit has none.
- Approval appears in the existing approvals TUI as a `NoteAppend` proposal with path and the first
  line of the text. The TUI does not gain a note browser in this change. *Open question Q8: whether a
  note browser belongs in `haste-tui`.*

### D10. Synced notes folder
Appends are safe for one writer on one machine. When iCloud Drive or Obsidian Sync also writes a
file in the notes folder, a conflict copy may be created and an append may land on the losing side.
Only the notes folder matters; the rest of the vault is out of scope. The spikes check whether the
chosen folder is synced and measure the conflict inside it. Mitigations to choose from after the
spikes, not decided: write per-day files instead of appending to a shared one; put the folder outside
synced locations; or accept conflicts and surface them in `haste note log`. *Open question Q9,
narrowed to the notes folder only.*

### D11. Audit
Record shapes stay as they are: `note.append` with `written`, `denied` or `io_error`, path, and
`bytes=<n>`. New reason codes are plain identifiers (`hidden_path`, `unknown_project`,
`machine_principal`, `size_limit`, `notes_root_missing`, `outside_root`, `front_matter_mismatch`). No note text, no
identifiers beyond the Person or object id, and no addresses.

## Risks / Trade-offs

- Obsidian and iCloud may hold files open or rewrite them in the notes folder, so appends can race with the sync. Mitigated by the spikes and Q9, not solved here.
- Front-matter-once depends on the file being created by haste. A human-created person note is appended to, and the daemon refuses if its `haste_person` does not match. Trade-off: no automatic adoption of human notes.
- Text in approval proposals sits in the store until approved, rejected or expired. Acceptable under the existing proposal retention; no new retention rule here.
- `haste:event/<id>` depends on an unarchived change. Gated to avoid a dangling reference.

## Migration

Existing `HASTE_NOTES_ROOT` users keep their setting; it remains the single folder bounding writes. Existing
`people/` and `tasks/` files are unchanged; they gain no front matter, because the daemon never
rewrites existing files. No store migration is required if proposal payloads already support a new
variant; otherwise one forward-only migration is added.

## Open questions status

- Q1 (`.obsidian/` required): resolved, removed by the one-folder decision (D1).
- Q4 is resolved (default `auto`); the `NoteReference` follow-up is noted in D6. Q2 (slug collisions), Q3 (free text in task notes), Q5 (project people notes), Q6 (per-file cap), Q7 (daily count cap), Q8 (TUI note browser): still open.
- Q9 (sync conflicts): open, narrowed to the notes folder only (D10).
