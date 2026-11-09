# proposals Specification

## Purpose
TBD - created by syncing change core-policy-gate. Update Purpose after archive.

## Requirements

### Requirement: Non-blocking proposal creation
When a `pending` outcome is produced for an agent request, the daemon SHALL create a proposal and SHALL return its identifier and state `pending` immediately, without waiting for a human decision.

#### Scenario: Immediate return
- **WHEN** an agent requests a `draft` action
- **THEN** the CLI receives a proposal ID and state `pending` in the same invocation

### Requirement: Idempotency key
Every proposal creation request SHALL carry an idempotency key. The daemon SHALL treat a repeated `(principal, idempotency_key)` pair as the same request and SHALL return the existing proposal without creating a new one.

#### Scenario: Retried creation
- **WHEN** the same agent submits a creation request twice with the same idempotency key
- **THEN** exactly one proposal exists and both responses carry its identifier

#### Scenario: Missing idempotency key
- **WHEN** a creation request arrives without an idempotency key
- **THEN** the daemon rejects it with a validation error

### Requirement: Proposal lifecycle states
A proposal SHALL be in exactly one of the states `pending`, `approved`, `executing`, `sent`, `rejected`, `expired`, or `superseded`. Permitted transitions are: `pending` to `approved`, `pending` to `rejected`, `pending` to `expired`, `pending` to `superseded`, `approved` to `executing`, `approved` to `sent` (a draft sent through its connector), `approved` to `superseded`, `executing` to `sent`, and `executing` to `approved` when an execution failed before any external effect. All other transitions SHALL be refused.

#### Scenario: Illegal transition refused
- **WHEN** a request attempts to move a `rejected` proposal to `sent`
- **THEN** the daemon refuses the transition and leaves the state unchanged

#### Scenario: Expiry
- **WHEN** a `pending` proposal's TTL elapses without a human decision
- **THEN** its state becomes `expired` and the originating agent is notified

### Requirement: Approve and send are separate human actions
Approval and sending SHALL be distinct actions. Approving a proposal SHALL NOT perform the external send. Sending SHALL require a proposal already in state `approved`. Both actions SHALL be executable only by a human principal through the TUI or the human shell, never by agent tokens.

#### Scenario: Approve does not send
- **WHEN** a human approves a proposal
- **THEN** the state becomes `approved` and no connector send is invoked

#### Scenario: Send requires approval
- **WHEN** a human requests send for a proposal in state `pending`
- **THEN** the request is refused

#### Scenario: Agent cannot approve
- **WHEN** an agent token requests approval of any proposal
- **THEN** the request is denied and audited

### Requirement: Stale-thread detection
Each proposal that references a thread SHALL record the thread version at creation. Before approval and before send, the daemon SHALL compare that version to the current thread version. If they differ, the daemon SHALL set the `stale` flag on the proposal and SHALL require explicit human acknowledgement before the approval or send proceeds.

#### Scenario: Thread changed after proposal
- **WHEN** a thread receives a new message after a draft proposal referencing it was created
- **THEN** the proposal is flagged `stale` and approval requires acknowledgement

#### Scenario: Thread unchanged
- **WHEN** the referenced thread version still equals the recorded version
- **THEN** no stale flag is set

### Requirement: Supersession
When a new proposal is created with the same principal, action, and source thread as an existing `pending` or `approved` proposal, the older proposal SHALL transition to `superseded`.

#### Scenario: Newer draft supersedes older
- **WHEN** an agent creates a second draft proposal for the same thread while the first is still `pending`
- **THEN** the first proposal becomes `superseded` and the second is `pending`

### Requirement: Result delivery to the agent
When a proposal reaches `sent`, `rejected`, or `expired`, the daemon SHALL deliver a short result message to the originating agent through herdr `agent.prompt`: over the herdr socket when the proposal's `origin_machine` is `local` or absent, and through `herdr --machine <label> agent prompt` when it is a remote machine. The herdr agent name SHALL be derived from the principal id by the mapping in `herdr-link`, and a principal that does not map SHALL NOT receive a prompt. Delivery SHALL be attempted at most three times with backoff, and each failure SHALL be recorded in the audit log. While the local herdr link is disconnected or in protocol mismatch, delivery to a local agent SHALL use the same three-attempt policy through the plain herdr CLI.

#### Scenario: Local delivery succeeds
- **WHEN** a proposal from a local agent is rejected and the agent pane is reachable
- **THEN** the agent receives one prompt over the socket containing the proposal ID and the result

#### Scenario: Remote delivery goes through the hub
- **WHEN** a proposal whose `origin_machine` is `m1` is rejected
- **THEN** the prompt is sent with `herdr --machine m1 agent prompt` and not over the local socket

#### Scenario: Delivery fails repeatedly
- **WHEN** three delivery attempts fail
- **THEN** no further attempts are made and the failure is recorded, and the result remains available through `haste proposal status`

#### Scenario: Unmappable principal
- **WHEN** a proposal result is due for a principal id that is not a valid herdr agent name
- **THEN** no prompt is sent, the refusal is audited, and the result remains available through `haste proposal status`

#### Scenario: Socket down uses fallback
- **WHEN** the local herdr link is disconnected and a result is due for a local agent
- **THEN** delivery uses the plain CLI fallback with the same three-attempt limit

### Requirement: Status query
The `haste proposal status <id>` command SHALL return the current state, stale flag, timestamps, and result for a proposal owned by the calling principal. An agent SHALL NOT be able to read another principal's proposals.

#### Scenario: Own proposal
- **WHEN** an agent queries status for a proposal it created
- **THEN** the current state and result are returned

#### Scenario: Foreign proposal
- **WHEN** an agent queries status for a proposal created by another principal
- **THEN** the daemon returns a not-found error

### Requirement: Project attribution from the agent's workspace
A proposal SHALL carry an optional `project` and an optional `origin_machine`. When the agent CLI supplies a machine hint (`HASTE_MACHINE`, default `local`) and a pane hint (`HERDR_PANE_ID`), the daemon SHALL look the pane up in its status model for that machine and, if it is found, SHALL store the pane's workspace label as `project` and the machine as `origin_machine`. If the machine or pane is unknown, the proposal SHALL be created without a project and without error. The hints SHALL be used only for routing and attribution and SHALL NOT affect authentication, the capability of the principal, or the gate outcome.

#### Scenario: Workspace becomes the project
- **WHEN** an agent in the pane `w1:p1` of workspace `haste` on machine `local` creates a proposal with those hints
- **THEN** the proposal stores `project` = `haste` and `origin_machine` = `local`

#### Scenario: Unknown pane gives no project
- **WHEN** the pane hint matches no pane in the machine's status model
- **THEN** the proposal is created with no project and no error

#### Scenario: Hints do not grant anything
- **WHEN** an agent sends hints naming another agent's workspace
- **THEN** its identity, capability and gate outcome are unchanged and only its own proposal's project label differs

### Requirement: Reminder proposal payloads
The proposal store SHALL support two additional payloads: `ReminderCreate` with a title, an optional due timestamp, an optional list name, an optional `project` and an optional thread reference, and `ReminderComplete` with a reminder reference and an optional `project`. Both SHALL follow the same non-blocking, idempotent creation rules as the existing payloads.

#### Scenario: Reminder completion proposed by an agent
- **WHEN** an agent creates a `ReminderComplete` proposal with an idempotency key
- **THEN** the proposal is created in state `pending` and returns its identifier immediately

#### Scenario: Repeated reminder proposal
- **WHEN** the same agent submits the same `ReminderCreate` with the same idempotency key twice
- **THEN** exactly one proposal exists

### Requirement: Reminder create project becomes the reminder tag
When a `ReminderCreate` proposal carrying a `project` is executed, the created reminder SHALL receive that project as its scope tag, and the tag SHALL be accepted only if it is in the known tag set at execution time.

#### Scenario: Known project tag applied
- **WHEN** an approved create with `project = haste` executes and `haste` is a known tag
- **THEN** the new reminder carries the scope tag `haste`

#### Scenario: Unknown project refused
- **WHEN** an approved create carries a project that is not in the known tag set
- **THEN** execution is refused and the proposal stays `approved`

### Requirement: Calendar proposal payloads
The proposal system SHALL support two calendar payloads: `CalendarCreate` with calendar, title, start, end and optional attendees and description, and `CalendarUpdate` with an event reference and changed fields limited to title, start, end, description and location. A proposal from an agent SHALL record the herdr workspace name as its project.

#### Scenario: Create proposal recorded
- **WHEN** an agent in a herdr workspace proposes a calendar create
- **THEN** a pending `CalendarCreate` proposal is stored with the workspace as project and nothing is written to Google Calendar

#### Scenario: Update with disallowed field rejected
- **WHEN** an agent proposes a `CalendarUpdate` naming a field outside the allow-list
- **THEN** the proposal is rejected at creation and not stored as pending

### Requirement: Calendar proposals execute only on human approval
Approval of a calendar proposal SHALL be a separate human action from creating the proposal. Execution SHALL happen only on approval and SHALL be audited with the Google event ID as detail.

#### Scenario: Approval executes once
- **WHEN** the human approves a calendar proposal and then runs it
- **THEN** the connector executes it once (the proposal moves through `executing` to `sent`) and the proposal records the Google event id as its result; an unknown outcome stays `executing` and is never re-run automatically

#### Scenario: Rejected proposal executes nothing
- **WHEN** the human rejects a calendar proposal
- **THEN** no calendar request is made
