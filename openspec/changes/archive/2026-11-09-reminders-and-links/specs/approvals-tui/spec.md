## ADDED Requirements

### Requirement: Reminder proposals are shown with title and project
The approvals view SHALL list `ReminderCreate` and `ReminderComplete` proposals with their kind, title or reminder title, and project, and SHALL NOT display notes or thread bodies in the list.

#### Scenario: Completion proposal shown
- **WHEN** a `ReminderComplete` proposal is pending
- **THEN** the approvals view shows it with the reminder title and the agent's project

### Requirement: Human-only reminder execution actions
Approving a reminder proposal and requesting its execution SHALL be human-only actions in the approvals view, and execution SHALL be a separate action from approval, as for sends.

#### Scenario: Approve does not write to Reminders
- **WHEN** the human approves a `ReminderCreate` proposal
- **THEN** the state becomes `approved` and no helper call is made

#### Scenario: Execution requires a separate action
- **WHEN** the human requests execution of an approved reminder proposal
- **THEN** the proposal moves through `executing` to `sent` or back to `approved`

### Requirement: Reminders permission status in the status view
The status view SHALL show the Reminders connector state, including `permission needed`, the kill-switch state, and the last sync cycle counts, and SHALL NOT show titles or notes.

#### Scenario: Permission needed shown
- **WHEN** the Reminders status is not granted
- **THEN** the status view shows `permission needed` and no sync is attempted

### Requirement: Proposals awaiting a check after a crash
The approvals view SHALL show a proposal in state `executing` at daemon start as needing a human check, with the option to mark it `sent` or `approved` after the human verifies the result in Reminders.

#### Scenario: Executing proposal after restart
- **WHEN** the daemon restarts with a proposal in state `executing`
- **THEN** the approvals view lists it as needing a check
