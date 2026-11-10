## MODIFIED Requirements

### Requirement: Human-only send action
The approvals view SHALL offer a send action for proposals in state `approved`, separate from approve. Send SHALL show every recipient, the source and the subject, SHALL display the recipient digest the daemon will verify, and SHALL run only after a fixed 5-second delay during which any key cancels it; a stale thread, or recipients that changed after the preview, SHALL require `y` instead of the timer. The send action SHALL NOT be offered for proposals in other states, and SHALL NOT be offered for a proposal whose source has no send-capable connector.

#### Scenario: Send requires approved state
- **WHEN** the user selects a `pending` proposal
- **THEN** the send action is not available

#### Scenario: Send confirmation lists recipients
- **WHEN** the user invokes send on an `approved` proposal
- **THEN** a preview listing every recipient, the source and the subject is shown with a 5-second countdown before the daemon is asked to send, and any key cancels it

#### Scenario: Recipients changed
- **WHEN** the daemon answers `recipients_changed` to a confirmed send
- **THEN** the view reloads the preview and shows the new recipients, and nothing is sent

## ADDED Requirements

### Requirement: Sends needing a check
The approvals view SHALL list a send proposal in state `executing` as needing a check, showing its source, its subject, and the audit outcome label, and SHALL offer two human actions: mark it `sent` after the human verified the mail in the Sent folder or the app, and return it to `approved` after the human verified nothing was delivered. Each choice SHALL be audited as a human override. The view SHALL NOT offer to re-send an `executing` proposal directly.

#### Scenario: Ambiguous send listed
- **WHEN** a send proposal is `executing` after an ambiguous outcome
- **THEN** the view lists it under needs-check with the outcome label `outcome_unknown` and offers only the two checked actions

#### Scenario: Human marks sent
- **WHEN** the human marks an executing send as `sent`
- **THEN** the proposal becomes `sent`, and an audit entry records the human override

#### Scenario: No direct re-send
- **WHEN** the human views an `executing` send
- **THEN** no send action is offered for it
