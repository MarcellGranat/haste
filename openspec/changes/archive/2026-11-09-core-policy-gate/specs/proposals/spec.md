## ADDED Requirements

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
A proposal SHALL be in exactly one of the states `pending`, `approved`, `sent`, `rejected`, `expired`, or `superseded`. Permitted transitions are: `pending` to `approved`, `pending` to `rejected`, `pending` to `expired`, `pending` to `superseded`, `approved` to `sent`, `approved` to `superseded`. All other transitions SHALL be refused.

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
When a proposal reaches `sent`, `rejected`, or `expired`, the daemon SHALL deliver a short result message to the originating agent through herdr `agent.prompt`. Delivery SHALL be attempted at most three times with backoff, and each failure SHALL be recorded in the audit log.

#### Scenario: Delivery succeeds
- **WHEN** a proposal is rejected and the agent pane is reachable
- **THEN** the agent receives one prompt containing the proposal ID and the result

#### Scenario: Delivery fails repeatedly
- **WHEN** three delivery attempts fail
- **THEN** no further attempts are made and the failure is recorded, and the result remains available through `haste proposal status`

### Requirement: Status query
The `haste proposal status <id>` command SHALL return the current state, stale flag, timestamps, and result for a proposal owned by the calling principal. An agent SHALL NOT be able to read another principal's proposals.

#### Scenario: Own proposal
- **WHEN** an agent queries status for a proposal it created
- **THEN** the current state and result are returned

#### Scenario: Foreign proposal
- **WHEN** an agent queries status for a proposal created by another principal
- **THEN** the daemon returns a not-found error
