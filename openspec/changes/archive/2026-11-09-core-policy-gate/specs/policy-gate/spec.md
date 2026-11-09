## ADDED Requirements

### Requirement: Every action resolves to a gate outcome
Every agent or machine request for an action SHALL be evaluated by the policy gate and SHALL resolve to exactly one outcome: `auto`, `pending`, or `denied`. The gate SHALL be keyed by principal (agent or herdr machine) and action.

#### Scenario: Read is automatic
- **WHEN** an agent requests the `read` action
- **THEN** the outcome is `auto` and the request is executed

#### Scenario: Draft requires approval
- **WHEN** an agent requests the `draft` action
- **THEN** the outcome is `pending` and a proposal is created

#### Scenario: Archive requires approval
- **WHEN** an agent requests the `archive` action on a thread
- **THEN** the outcome is `pending`, a proposal is created, and no triage change is applied until a human approves

#### Scenario: Snooze requires approval
- **WHEN** an agent requests the `snooze` action on a thread
- **THEN** the outcome is `pending`, a proposal is created, and `snoozed_until` is not changed until a human approves

#### Scenario: Reminder creation requires approval
- **WHEN** an agent requests the `reminder.create` action
- **THEN** the outcome is `pending` and a proposal is created

### Requirement: Archive and snooze have no automatic execution and no undo log
The system SHALL NOT execute `archive` or `snooze` automatically for any principal, and SHALL NOT maintain an undo log for them.

#### Scenario: No auto archive configuration
- **WHEN** any principal's capability entry is evaluated for `archive`
- **THEN** the outcome is never `auto`

### Requirement: Note append is auto only inside allow-listed paths
The `note.append` action SHALL resolve to `auto` only when the canonical target path lies under `people/**` or `tasks/**` relative to the notes root. Any other target path SHALL resolve to `denied`.

#### Scenario: Append inside allow-list
- **WHEN** an agent requests `note.append` for `people/ana.md`
- **THEN** the outcome is `auto`

#### Scenario: Append outside allow-list
- **WHEN** an agent requests `note.append` for `private/keys.md`
- **THEN** the outcome is `denied` and nothing is written

#### Scenario: Path traversal attempt
- **WHEN** an agent requests `note.append` for `people/../private/keys.md`
- **THEN** the canonical path is outside the allow-list, the outcome is `denied`, and nothing is written

### Requirement: Note append is daemon-performed, append-only, and attributed
The daemon SHALL perform every `note.append` write itself. It SHALL open the target with append-only and no-follow semantics, SHALL write an attribution comment naming the agent and timestamp before the appended text, and SHALL NOT expose any edit or delete action for notes to agents.

#### Scenario: Attributed append
- **WHEN** an allowed `note.append` is executed for agent `agent-a`
- **THEN** the file gains a line `<!-- haste: appended by agent-a at <timestamp> -->` followed by the requested text, and no existing bytes are modified

#### Scenario: Agent requests edit or delete
- **WHEN** an agent token requests a note edit or delete action
- **THEN** the daemon responds with `denied` because no such action exists for agents

### Requirement: Path escape and symlink checks
Before writing, the daemon SHALL canonicalize the target path, SHALL reject any path whose canonical form is outside the allow-listed prefixes, and SHALL reject the write if any path component is a symbolic link.

#### Scenario: Symlink in path component
- **WHEN** `people/` contains a symlink named `link` pointing outside the notes root and an agent requests `note.append` for `people/link/x.md`
- **THEN** the outcome is `denied` and nothing is written

#### Scenario: Symlink target file
- **WHEN** the target file `tasks/todo.md` is itself a symlink
- **THEN** the write is rejected and an audit record is written

### Requirement: Send is not an agent action
The `send` action SHALL NOT be present in the agent CLI command set. The daemon SHALL deny `send` for every agent and machine principal regardless of request content. `send` SHALL only be executable through a human TUI action.

#### Scenario: Agent CLI has no send verb
- **WHEN** the `haste` CLI is invoked with a send subcommand under an agent token
- **THEN** the command is not recognized

#### Scenario: Forged send request from agent token
- **WHEN** a request with a valid agent token and action `send` is received by the daemon
- **THEN** the outcome is `denied` and the attempt is audited

### Requirement: Capability matrix is per principal
The capability matrix SHALL be keyed by principal, where principal is either an agent identifier or a herdr machine identifier. A principal with no entry for an action SHALL resolve that action to `denied`.

#### Scenario: Unknown action
- **WHEN** a principal requests an action absent from its matrix entry
- **THEN** the outcome is `denied`

#### Scenario: Machine principal limited to read
- **WHEN** a herdr machine principal requests `draft`
- **THEN** the outcome is `denied` unless the matrix grants it, and the decision is audited

### Requirement: Gate decisions are pure and exhaustively testable
The gate evaluation function SHALL depend only on its inputs (principal, action, target, matrix) and SHALL have no side effects other than the audit write performed by its caller.

#### Scenario: Matrix test coverage
- **WHEN** the test suite runs
- **THEN** every `(action, outcome)` pair in the decided matrix is asserted by at least one test
