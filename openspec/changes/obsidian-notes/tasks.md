## 0. Human spikes (before any code)

- [ ] 0.1 (human) Locate the Obsidian vault on this Mac: run `find ~ -maxdepth 4 -name .obsidian -type d 2>/dev/null` and record the vault folder path in `design.md` Context. Do not open or read note contents.
- [ ] 0.2 (human) Determine sync: check whether the vault path is under `~/Library/Mobile Documents/` (iCloud Drive), and whether Obsidian Sync is enabled in Obsidian settings. Record the answer in `design.md` (Q9 input).
- [ ] 0.3 (human) Conflict experiment in a throwaway folder created inside the synced location and deleted afterwards: append a line to one file from two shells in quick succession, then check whether iCloud or Obsidian creates a conflict copy. Record the result in `design.md` (D10).
- [ ] 0.4 (human) Decide how the daemon learns `HASTE_NOTES_ROOT` under launchd (plist environment key or `notes.json`), and record it as a decision in `design.md` (D1).
- [ ] 0.5 Confirm with the human that agents on remote herdr machines are out of scope and that the hub alone writes the vault. Record the confirmation in `design.md` Non-Goals.
- [ ] 0.6 Answer or explicitly defer open questions Q1 to Q9 in `design.md` with the human, and record each answer or a "deferred" note. Do not implement a default for a deferred question.

## 1. Vault configuration and path rules

- [ ] 1.1 Load the vault root from the configured source, require it to exist as a directory, and expose a `vault_missing` denial when it does not
- [ ] 1.2 Extend `note_path_allowed` (haste-core) with the `projects/<project>/` rule, the hidden-component rule, the `.md` suffix rule, and the rejection of absolute, `..`, backslash, NUL, and `~` paths
- [ ] 1.3 Pass the set of known project slugs (from the store's workspace labels) into the path predicate as an explicit argument, keeping the function pure
- [ ] 1.4 Add unit tests: each rejected form, each accepted form, a project slug that is not known, and a bare `projects/` path

## 2. Body and size checks

- [ ] 2.1 Implement the body check (no `---` line, no `<!-- haste:` line, no NUL, valid UTF-8) and its `body_rejected` reason
- [ ] 2.2 Implement the per-file size check from metadata before writing, with the cap chosen in Q6 (use a named constant)
- [ ] 2.3 Keep the existing 64 KiB per-append limit as a named constant and enforce it before opening the file
- [ ] 2.4 Add tests in a tempdir: oversized append, file at cap, injection and spoof bodies, each leaving bytes unchanged

## 3. Append executor

- [ ] 3.1 Keep `O_APPEND|O_NOFOLLOW` and the attribution comment; add the reason codes `hidden_path`, `unknown_project`, `size_limit`, `body_rejected`, `symlink`, `vault_missing`, `front_matter_mismatch`
- [ ] 3.2 Add a test that every existing byte of a file is unchanged after any sequence of appends, using a tempdir
- [ ] 3.3 Add a test that a symlinked component or target is refused with `symlink`

## 4. Person notes

- [ ] 4.1 Implement the slug function (ASCII fold, lowercase, non-alphanumeric runs to `-`, trim, 60-character cap) and unit-test it on accented names, punctuation-only names, and long names
- [ ] 4.2 Implement front matter creation on first write only, with `haste_person` and `created`; aliases and addresses only behind the settings `person_notes.aliases` and `person_notes.include_addresses` (default off)
- [ ] 4.3 Implement the `front_matter_mismatch` check for an existing file whose `haste_person` differs or is missing
- [ ] 4.4 Implement the dated heading `### <YYYY-MM-DD HH:MM>` in local time followed by the attribution comment and text
- [ ] 4.5 Add tests in a tempdir: first entry creates front matter, second entry keeps it, mismatch refused, default front matter contains no address, opted-in alias contains no address, unknown person id gives `NotFound`
- [ ] 4.6 Resolve Q2 (slug collision) before implementing collision handling; add tests only for the chosen rule

## 5. Work back-reference notes

- [ ] 5.1 Add the typed `NoteReference` request (`kind`, `id`) to `haste-proto`, with no text field, and make unknown fields a decode error
- [ ] 5.2 Implement identifier validation (ASCII, bounded length, store charset) and the existence check through the store
- [ ] 5.3 Compose the single line `- haste:<kind>/<id>` in the daemon and write it under the dated heading in `tasks/<topic-slug>.md` or `projects/<project>/tasks/<topic-slug>.md`
- [ ] 5.4 Gate the `event` kind on the calendar event model (`unknown_kind` otherwise); leave the kind out until the `google-calendar` change is archived
- [ ] 5.5 Add tests in a tempdir: thread, reminder and event references; missing object; malformed id; a reminder title and an email subject never appear in the file

## 6. Policy gate and approval mode

- [ ] 6.1 Update the `note.append` outcome in the pure `evaluate` function to use the new path predicate, and add `pending` for `approval` mode
- [ ] 6.2 Add the machine principal rule that always denies `note.append` with reason `machine_principal`, outside the matrix
- [ ] 6.3 Add the per-principal `note_mode` (default as decided in Q4), stored with the principal record, changeable only through the human CLI
- [ ] 6.4 Extend the exhaustive gate test so every `(principal kind, note mode, path class)` combination has an asserted outcome
- [ ] 6.5 Add the `NoteAppend` proposal payload to `haste-store` and `haste-core`, with the executor path on approval and the duplicate-approval guard
- [ ] 6.6 Add tests: approval mode creates a pending proposal and writes nothing; approval appends once; rejection and expiry write nothing; a machine principal never produces a proposal

## 7. Human read surfaces

- [ ] 7.1 Add the human-only `note show <path>` request and CLI verb (feature `human`), refusing hidden paths and any principal other than `human`
- [ ] 7.2 Add the human-only `note log [--since]` request and verb that prints audit rows without text
- [ ] 7.3 Add the `NoteAppend` rendering to the approvals TUI showing path and first line of text, if Q8 keeps this in scope
- [ ] 7.4 Add tests: agent token denied for `note show`; `note log` output contains no text from the appended body

## 8. Audit and configuration

- [ ] 8.1 Ensure every note outcome writes exactly one audit row with path, outcome, and `bytes=<n>` or a reason code, and no text, address, or subject
- [ ] 8.2 Load and validate `person_notes.aliases`, `person_notes.include_addresses`, and the note mode defaults, rejecting unknown keys
- [ ] 8.3 Add a test that the audit file contains no body text, using a fixed sentinel string in the appended text

## 9. Documentation

- [ ] 9.1 Add a README section "Obsidian vault" covering the vault folder setting, vault-relative paths, hidden-path rule, person notes, back-reference notes, approval mode, and the `note show` and `note log` verbs
- [ ] 9.2 Document the sync findings from group 0 and the chosen mitigation for Q9 in the README

## 10. Quality checks

- [ ] 10.1 Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` and fix all failures
- [ ] 10.2 Confirm every test uses a tempdir or a fake; no test touches the real vault path or the real data directory

## 11. Manual smoke (human, live vault)

- [ ] 11.1 (human) Set the vault setting to a throwaway folder inside the vault location, start `hasted`, and confirm `haste note show` works for the human and is refused for an agent token
- [ ] 11.2 (human) As an agent in `auto` mode, append to `people/<slug>.md` for a real person id; open the note in Obsidian and confirm front matter appears once and the entry is dated
- [ ] 11.3 (human) Append a second entry and confirm no earlier bytes changed (compare with a copy taken before the append)
- [ ] 11.4 (human) Record a thread and a reminder reference in a task note; confirm the note contains only `haste:` identifier lines
- [ ] 11.5 (human) Switch the agent to `approval` mode, request an append, confirm nothing is written, approve in the TUI, and confirm exactly one entry appears
- [ ] 11.6 (human) Confirm `haste note log` lists the writes with byte counts and no text
- [ ] 11.7 (human) Delete the throwaway folder in Obsidian and in the synced location, and check the sync result from group 0 again
