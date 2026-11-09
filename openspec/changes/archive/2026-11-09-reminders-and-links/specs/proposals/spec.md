## MODIFIED Requirements

### Requirement: Proposal lifecycle states
A proposal SHALL be in exactly one of the states `pending`, `approved`, `executing`, `sent`, `rejected`, `expired`, or `superseded`. Permitted transitions are: `pending` to `approved`, `pending` to `rejected`, `pending` to `expired`, `pending` to `superseded`, `approved` to `executing`, `approved` to `sent` (a draft sent through its connector), `approved` to `superseded`, `executing` to `sent`, and `executing` to `approved` when an execution failed before any external effect. All other transitions SHALL be refused.

#### Scenario: Illegal transition refused
- **WHEN** a request attempts to move a `rejected` proposal to `sent`
- **THEN** the daemon refuses the transition and leaves the state unchanged

#### Scenario: Expiry
- **WHEN** a `pending` proposal's TTL elapses without a human decision
- **THEN** its state becomes `expired` and the originating agent is notified

## ADDED Requirements

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
