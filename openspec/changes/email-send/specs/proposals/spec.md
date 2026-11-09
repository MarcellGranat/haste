## MODIFIED Requirements

### Requirement: Proposal lifecycle states
A proposal SHALL be in exactly one of the states `pending`, `approved`, `executing`, `sent`, `rejected`, `expired`, or `superseded`. Permitted transitions are: `pending` to `approved`, `pending` to `rejected`, `pending` to `expired`, `pending` to `superseded`, `approved` to `executing`, `approved` to `superseded`, `executing` to `sent`, and `executing` to `approved` when an execution failed before any external effect. `approved` to `sent` SHALL NOT be a permitted transition: every external write, including a send, passes through `executing`. All other transitions SHALL be refused.

#### Scenario: Illegal transition refused
- **WHEN** a request attempts to move a `rejected` proposal to `sent`
- **THEN** the daemon refuses the transition and leaves the state unchanged

#### Scenario: Direct approved to sent refused
- **WHEN** a request attempts to move an `approved` proposal directly to `sent`
- **THEN** the transition is refused and the state stays `approved`

#### Scenario: Expiry
- **WHEN** a `pending` proposal's TTL elapses without a human decision
- **THEN** its state becomes `expired` and the originating agent is notified

## ADDED Requirements

### Requirement: Send execution passes through executing
A send SHALL move its proposal from `approved` to `executing` before any connector call. A send that succeeds SHALL move it from `executing` to `sent`. A send whose outcome is certainly without effect SHALL move it from `executing` back to `approved` with the reason recorded. A send whose outcome is ambiguous SHALL leave it in `executing`. Ambiguous sends SHALL NOT be re-run automatically.

#### Scenario: Successful send path
- **WHEN** the human sends an approved proposal and the connector accepts it
- **THEN** the proposal moves `approved` to `executing` to `sent`, with the provider id recorded as result

#### Scenario: Ambiguous send stays executing
- **WHEN** a send's outcome is ambiguous
- **THEN** the proposal is `executing`, and no later daemon action moves it to `approved` or `sent`

#### Scenario: Failed send returns to approved
- **WHEN** a send's outcome is certainly without effect
- **THEN** the proposal is `approved` again and the reason is in the audit log

### Requirement: Executing sends are a human check after restart
At daemon start, every proposal in state `executing` SHALL be left unchanged and reported as needing a human check, whether it is a send or a reminder or calendar write. The daemon SHALL NOT perform any external action for it.

#### Scenario: Restart with executing send
- **WHEN** the daemon starts and a send proposal is `executing`
- **THEN** the proposal stays `executing`, no connector call is made, and the approvals view lists it as needing a check
