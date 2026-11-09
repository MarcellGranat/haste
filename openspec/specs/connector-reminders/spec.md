## Purpose
Apple Reminders connector: a read-only mirror through an EventKit helper, plus approval-only create and complete. Never deletes or edits.

## Requirements

### Requirement: Reminders connector capability declaration
The Reminders connector SHALL declare the capabilities `read = true`, `reminders_create` and `reminders_complete` as configured by the human, and `draft = false`, `send = false`, `archive = false`. It MUST NOT expose any method that deletes a reminder or changes a reminder's title, notes, due date or list.

#### Scenario: Capabilities reported
- **WHEN** the daemon queries the Reminders connector capabilities
- **THEN** `draft`, `send` and `archive` are false and no delete or edit method is present

### Requirement: Helper request contract
The daemon SHALL invoke the Reminders helper executable with exactly one request, expressed as a closed typed enum with the variants `Authorization`, `ListLists`, `FetchChanged`, `CreateReminder` and `CompleteReminder`. The request SHALL be written to the helper's stdin, which SHALL then be closed. The daemon SHALL enforce a hard timeout per call, SHALL kill the helper's whole process group on timeout, and SHALL accept only a single JSON object on stdout.

#### Scenario: Hung helper is killed
- **WHEN** the helper does not exit within the configured timeout
- **THEN** its process group is killed, the call is reported as transient, and no output is accepted

#### Scenario: Malformed helper output
- **WHEN** the helper prints output that is not one JSON object matching the expected response
- **THEN** the call fails with a non-transient error and no reminder is written

#### Scenario: No delete or edit request exists
- **WHEN** the request type is inspected
- **THEN** it has no variant that deletes a reminder or edits its fields

### Requirement: Permission state is read without prompting in the daemon
The daemon SHALL only read the Reminders authorization status. It SHALL NOT trigger the macOS permission prompt. When the status is not granted, the connector SHALL record the state `permission needed`, SHALL NOT retry, and SHALL NOT trip the circuit breaker. Authorization requests that prompt SHALL be available only through the human-run `haste connector reminders auth` command.

#### Scenario: Permission denied in the daemon
- **WHEN** a scheduled cycle finds the Reminders status not granted
- **THEN** the cycle records `permission needed`, makes no fetch call, and performs no retry

#### Scenario: Human grants permission
- **WHEN** the human runs `haste connector reminders auth` from a terminal and access is granted
- **THEN** the status becomes granted and the next scheduled cycle may fetch

#### Scenario: Agent cannot prompt
- **WHEN** an agent principal requests the auth command or its effect
- **THEN** the request is denied and audited

### Requirement: Incremental scheduled sync
The connector SHALL fetch reminders changed since a stored cursor on a scheduled cycle, in batches of a fixed maximum size. A reminder SHALL be marked missing only after a complete, untruncated sweep does not return it. A partial or failed sweep SHALL NOT mark any reminder missing.

#### Scenario: Batch limit continues next cycle
- **WHEN** a fetch returns a full batch
- **THEN** the cursor stops at the last returned item and the next cycle continues from it

#### Scenario: Partial sweep marks nothing missing
- **WHEN** a sweep fails midway
- **THEN** no reminder is marked `missing_since` and the cursor is not advanced past the last stored item

#### Scenario: Reappearing reminder is no longer missing
- **WHEN** a reminder marked missing is returned by a later sweep
- **THEN** its `missing_since` is cleared and its tags and links are unchanged

### Requirement: Reminder sync is audited with counts only
Each Reminders sync cycle SHALL write one audit record containing the connector name, the outcome, and the counts `fetched`, `created`, `updated` and `missing`. The audit record SHALL NOT contain reminder titles, notes, or list names.

#### Scenario: Cycle audit
- **WHEN** a sync cycle completes
- **THEN** exactly one audit record holds the counts and no titles, notes or list names

### Requirement: Reminder creation only through an approved proposal
A reminder SHALL be created in Reminders only when a `ReminderCreate` proposal is in state `approved` and a human has requested execution. The daemon SHALL move the proposal to `executing` before calling the helper, and to `sent` only after the helper returns a new external id. The new reminder's notes SHALL carry only the haste thread or proposal identifier, never subject, body or participant text.

#### Scenario: Create without approval refused
- **WHEN** an execution request targets a `ReminderCreate` proposal in state `pending`
- **THEN** the request is refused and no helper call is made

#### Scenario: Successful create records the reminder
- **WHEN** an approved create executes and the helper returns an external id
- **THEN** the proposal becomes `sent`, the reminder row exists with that external id, and the proposal's project becomes the reminder's scope tag

#### Scenario: Create failure keeps approval
- **WHEN** the helper call fails after the proposal is `executing`
- **THEN** the proposal returns to `approved` and the failure is audited with ids and no titles

### Requirement: Reminder completion only through an approved proposal
A reminder SHALL be marked completed in Reminders only when a `ReminderComplete` proposal is in state `approved` and a human has requested execution. A completion on a reminder that is missing or already completed SHALL be refused with a validation error and the proposal SHALL stay `approved`.

#### Scenario: Complete an already completed reminder
- **WHEN** an approved completion targets a reminder whose completed flag is true
- **THEN** it is refused with a validation error and the proposal remains `approved`

#### Scenario: Agent proposal of completion
- **WHEN** an agent proposes completion of a reminder with its workspace hint
- **THEN** a `ReminderComplete` proposal is created in state `pending` carrying the workspace name as `project`, and no helper call is made

### Requirement: No automatic re-run after a crash
If the daemon starts and finds a proposal in state `executing`, it SHALL NOT call the helper for that proposal. It SHALL show the proposal to the human as needing a check, so that a create is not duplicated.

#### Scenario: Crash during create
- **WHEN** the daemon restarts with a `ReminderCreate` proposal in state `executing`
- **THEN** no helper call is made and the approvals view shows the proposal as needing a check

### Requirement: Reminders connector is disabled until released
The Reminders connector's kill switch SHALL be engaged when the connector is first installed, and no sync, create or complete call SHALL run until the human releases it.

#### Scenario: First install
- **WHEN** the daemon starts with no stored kill-switch state for reminders
- **THEN** the switch is engaged and no Reminders call is made
