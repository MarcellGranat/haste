# policy-gate Specification

## Purpose
TBD - created by syncing change core-policy-gate. Update Purpose after archive.

## Requirements

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
The `note.append` action SHALL resolve to `auto` only when the canonical target path, relative to the notes root, lies inside the notes root and under `people/**`, under `tasks/**`, or under `projects/<project>/**` where `<project>` is the slug of a known herdr workspace label, and no path component starts with `.`. Any other target path, including any path that resolves outside the notes root, SHALL resolve to `denied`. For a principal in `approval` mode, an otherwise allowed append SHALL resolve to `pending` instead of `auto`.

#### Scenario: Append inside allow-list
- **WHEN** an agent requests `note.append` for `people/ana.md`
- **THEN** the outcome is `auto`

#### Scenario: Append outside allow-list
- **WHEN** an agent requests `note.append` for `private/keys.md`
- **THEN** the outcome is `denied` and nothing is written

#### Scenario: Path traversal attempt
- **WHEN** an agent requests `note.append` for `people/../private/keys.md`
- **THEN** the canonical path is outside the allow-list, the outcome is `denied`, and nothing is written

#### Scenario: Escape from the notes folder
- **WHEN** an agent requests `note.append` for `../elsewhere/x.md`, or for a path through a symlink pointing outside the notes root
- **THEN** the outcome is `denied` and nothing is written outside the notes root

#### Scenario: Hidden folder
- **WHEN** an agent requests `note.append` for `.obsidian/workspace.md`
- **THEN** the outcome is `denied` with reason `hidden_path`

#### Scenario: Approval mode
- **WHEN** an agent in `approval` mode requests `note.append` for `people/ana.md`
- **THEN** the outcome is `pending` and a `NoteAppend` proposal is created

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

### Requirement: Machine read scope is tag based
Machine principals SHALL have read actions narrowed to threads carrying a project tag in their own machine's last known workspace-label set. The gate SHALL continue to resolve each machine action to `auto`, `pending`, or `denied` as before; the tag scope applies to the read results returned after an allowed read and SHALL NOT itself produce `auto`.

#### Scenario: Read allowed but scoped
- **WHEN** a machine principal with read capability requests a thread list
- **THEN** the gate resolves the read as allowed and the returned set is limited to threads tagged with that machine's project labels

#### Scenario: Scope does not widen capability
- **WHEN** a machine principal has no entry for `draft`
- **THEN** the gate resolves `draft` to `denied` regardless of any tags on the target

### Requirement: Herdr-originated actions are gate-evaluated
Any action a herdr machine or agent principal can trigger through the herdr link, including the `agent.prompt` result delivery, SHALL be gate-evaluated under the principal's capability row. The link and the machine, pane and workspace hints SHALL NOT bypass the gate.

#### Scenario: Link event does not grant action
- **WHEN** a herdr status event arrives for a pane
- **THEN** no gate action is performed on behalf of the agent and no capability is inferred from the event

### Requirement: Reminder actions are pending for agents
The policy matrix SHALL resolve `reminder.create` and `reminder.complete` to `pending` for agent principals and to `denied` for any direct execution path. Neither action SHALL resolve to `auto` for any principal. A human principal's execution still goes through the approved-proposal state machine.

#### Scenario: Agent reminder completion is pending
- **WHEN** an agent principal requests `reminder.complete`
- **THEN** the outcome is `pending` and a proposal is created

#### Scenario: No automatic reminder action
- **WHEN** the matrix is evaluated for `reminder.create` or `reminder.complete` for any principal kind
- **THEN** the outcome is never `auto`

### Requirement: Reminder execution is not an agent action
Executing an approved reminder proposal SHALL NOT be available to agent principals, and an agent request to execute SHALL be denied and audited.

#### Scenario: Agent requests execution
- **WHEN** an agent principal requests execution of a reminder proposal
- **THEN** the request is denied and audited

### Requirement: Calendar actions are pending for agents
The policy gate SHALL evaluate `calendar.create` and `calendar.update` for agent principals as `pending`, never `auto`, and for machine principals (read-only) as `denied`. The gate SHALL NOT define a `calendar.delete` action.

#### Scenario: Agent calendar create pending
- **WHEN** an agent requests `calendar.create`
- **THEN** the outcome is `pending` and a proposal is created

#### Scenario: Agent calendar update pending
- **WHEN** an agent requests `calendar.update`
- **THEN** the outcome is `pending` and a proposal is created

#### Scenario: Calendar delete undefined
- **WHEN** any principal requests `calendar.delete`
- **THEN** the gate returns `denied` because the action is not defined

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
