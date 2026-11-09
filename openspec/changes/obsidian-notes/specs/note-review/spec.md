## ADDED Requirements

### Requirement: Human-only note read
The daemon SHALL serve note text only to the human principal, through a read request usable only by the human build of the CLI. Agent and machine principals SHALL be denied any note read request.

#### Scenario: Human reads a note
- **WHEN** the human runs `haste note show people/ana-lopez.md`
- **THEN** the daemon returns the file text as it is on disk and the read is audited with path only

#### Scenario: Path outside the notes folder
- **WHEN** the human runs `haste note show ../other.md`
- **THEN** the request is refused and nothing outside the notes folder is read

#### Scenario: Agent tries to read a note
- **WHEN** an agent token sends a note read request
- **THEN** the outcome is `denied` and the attempt is audited

### Requirement: Human-only audit view without text
`haste note log` SHALL list `note.append` audit rows as path, byte length, principal, outcome, and time. It SHALL NOT display note text, because the audit log contains none. Output SHALL be limited to the most recent rows, with a `--since` filter.

#### Scenario: Log lists writes
- **WHEN** the human runs `haste note log --since <ts>` after two appends by `agent-a`
- **THEN** two rows appear with path, `bytes=<n>`, and outcome `written`, and no text

### Requirement: Approval mode turns agent appends into proposals
Each agent principal SHALL have a `note_mode` of `auto` or `approval`. In `approval` mode a `note.append` request SHALL resolve to `pending`, create a `NoteAppend` proposal, and write nothing until a human approves it. Approval SHALL execute the same note executor under the original principal. Rejection or expiry SHALL write nothing.

#### Scenario: Approval mode write
- **WHEN** agent `agent-a` in `approval` mode appends "Met on Tuesday." for `p-123`
- **THEN** a pending `NoteAppend` proposal is created, the file is unchanged, and the proposal result is `pending` for the agent

#### Scenario: Human approves
- **WHEN** the human approves that proposal
- **THEN** the entry is appended once, the audit records `written`, and the proposal records the path and byte length only

#### Scenario: Human rejects
- **WHEN** the human rejects that proposal
- **THEN** nothing is written and the proposal is closed

### Requirement: Machine principals are never in approval mode
A herdr machine principal SHALL NOT have a `note_mode` and SHALL NOT produce note proposals. Its `note.append` requests SHALL be denied.

#### Scenario: Machine append
- **WHEN** a herdr machine principal requests `note.append`
- **THEN** the outcome is `denied` with reason `machine_principal` and no proposal is created

### Requirement: Mode changes are human-only
Changing an agent's `note_mode` SHALL be possible only through the human build of the CLI. Agents SHALL NOT be able to change their own mode.

#### Scenario: Agent sets own mode
- **WHEN** an agent token requests a change of `note_mode` to `auto`
- **THEN** the request is denied and the attempt is audited
