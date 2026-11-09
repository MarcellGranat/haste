## ADDED Requirements

### Requirement: Approvals view lists pending proposals
The TUI SHALL provide an approvals view listing proposals in state `pending` or `approved`, and SHALL update the list on daemon change events without requiring a manual refresh.

#### Scenario: New proposal appears
- **WHEN** an agent creates a proposal while the approvals view is open
- **THEN** the proposal appears in the list without user action

### Requirement: Vi-style navigation
The approvals view SHALL support `j` and `k` to move the selection, `g` and `G` to jump to the first and last items, `/` to filter, and `Enter` to open the detail for the selected proposal.

#### Scenario: Move selection
- **WHEN** the user presses `j` with a list of several proposals
- **THEN** the selection moves down by one item

### Requirement: Per-agent grouping
The approvals view SHALL group proposals by originating principal and SHALL allow collapsing and expanding each group.

#### Scenario: Collapse group
- **WHEN** the user collapses the group for `agent-a`
- **THEN** its proposals are hidden and the group header shows the count

### Requirement: Batch approval
The approvals view SHALL allow the user to mark multiple proposals and approve them in one action. Batch approval SHALL apply only to the approve action. Batch approval SHALL require a single explicit confirmation that displays the count. Proposals flagged `stale` SHALL be excluded from a batch unless the user acknowledges each one individually.

#### Scenario: Batch approve with confirmation
- **WHEN** the user marks three proposals and confirms batch approval
- **THEN** all three transition to `approved` and each transition is audited

#### Scenario: Stale excluded from batch
- **WHEN** a marked proposal is flagged `stale`
- **THEN** it is not approved by the batch and the user is shown why

### Requirement: Human-only send action
The approvals view SHALL offer a send action for proposals in state `approved`, separate from approve. Send SHALL require a confirmation that names the recipient and the source. The send action SHALL NOT be offered for proposals in other states.

#### Scenario: Send requires approved state
- **WHEN** the user selects a `pending` proposal
- **THEN** the send action is not available

#### Scenario: Send confirmation
- **WHEN** the user invokes send on an `approved` proposal
- **THEN** a confirmation naming recipient and source is shown before the daemon is asked to send

### Requirement: Reject action
The approvals view SHALL allow the user to reject a `pending` proposal, which transitions it to `rejected` and triggers result delivery to the originating agent.

#### Scenario: Reject pending
- **WHEN** the user rejects a pending proposal
- **THEN** its state becomes `rejected` and the result is queued for delivery
