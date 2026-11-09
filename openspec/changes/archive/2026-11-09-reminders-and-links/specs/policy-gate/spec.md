## ADDED Requirements

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
