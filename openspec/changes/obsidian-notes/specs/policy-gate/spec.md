## MODIFIED Requirements

### Requirement: Note append is auto only inside allow-listed paths
The `note.append` action SHALL resolve to `auto` only when the canonical target path, relative to the vault root, lies under `people/**`, under `tasks/**`, or under `projects/<project>/**` where `<project>` is the slug of a known herdr workspace label, and no path component starts with `.`. Any other target path SHALL resolve to `denied`. For a principal in `approval` mode, an otherwise allowed append SHALL resolve to `pending` instead of `auto`.

#### Scenario: Append inside allow-list
- **WHEN** an agent requests `note.append` for `people/ana.md`
- **THEN** the outcome is `auto`

#### Scenario: Append outside allow-list
- **WHEN** an agent requests `note.append` for `private/keys.md`
- **THEN** the outcome is `denied` and nothing is written

#### Scenario: Path traversal attempt
- **WHEN** an agent requests `note.append` for `people/../private/keys.md`
- **THEN** the canonical path is outside the allow-list, the outcome is `denied`, and nothing is written

#### Scenario: Hidden folder
- **WHEN** an agent requests `note.append` for `.obsidian/workspace.md`
- **THEN** the outcome is `denied` with reason `hidden_path`

#### Scenario: Approval mode
- **WHEN** an agent in `approval` mode requests `note.append` for `people/ana.md`
- **THEN** the outcome is `pending` and a `NoteAppend` proposal is created

## ADDED Requirements

### Requirement: Machine principals never append notes
The gate SHALL resolve `note.append` to `denied` for every herdr machine principal, regardless of the capability matrix content, and SHALL audit the decision. This rule SHALL NOT be configurable.

#### Scenario: Machine principal with a matrix entry
- **WHEN** a machine principal has a matrix entry that lists `note.append`
- **THEN** the outcome is still `denied` with reason `machine_principal`

### Requirement: Note mode is per agent and not self-service
The gate SHALL read an agent's `note_mode` (`auto` or `approval`) from its principal record. No `note_mode` value SHALL resolve `note.append` to `auto` for a principal not permitted `note.append` in its matrix row.

#### Scenario: Mode without permission
- **WHEN** an agent without `note.append` in its matrix row has `note_mode` set to `auto`
- **THEN** the outcome is `denied`
