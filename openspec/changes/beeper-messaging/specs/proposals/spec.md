## ADDED Requirements

### Requirement: Beeper draft proposals target an existing chat
A reply to a Beeper chat SHALL be stored as a `Draft` proposal whose source thread is the Beeper chat thread and whose `to` is empty. Approving such a proposal SHALL mark it approved and SHALL NOT create any draft or message at the provider.

#### Scenario: Agent proposes a reply
- **WHEN** an agent proposes a reply to a Beeper chat thread
- **THEN** a pending `Draft` proposal is stored and nothing is sent to Beeper

#### Scenario: Approval creates nothing at the provider
- **WHEN** the human approves a Beeper draft proposal
- **THEN** the proposal becomes `approved`, no request is made to Beeper, and the audit entry is `draft.approve` with the Beeper source

### Requirement: Beeper send result records the message id
A successful Beeper send SHALL set the proposal result to `sent:beeper:<message id>`, and an ambiguous send SHALL keep the proposal in `executing` with no result.

#### Scenario: Successful send result
- **WHEN** Beeper accepts a send and returns a message id
- **THEN** the proposal state is `sent` and its result is `sent:beeper:<message id>`

#### Scenario: Ambiguous send has no result
- **WHEN** a Beeper send outcome is ambiguous
- **THEN** the proposal state is `executing` and no result is recorded
