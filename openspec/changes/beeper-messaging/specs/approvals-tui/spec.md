## ADDED Requirements

### Requirement: Chat send confirmation names the chat and the text
For a Beeper proposal, the send confirmation SHALL show the chat name, the network, the full text as it will be sent and the digest, and SHALL require `y` before the daemon is asked to send. The confirmation SHALL NOT be shown for a proposal whose chat is not selected.

#### Scenario: Confirmation content
- **WHEN** the user invokes send on an approved WhatsApp proposal
- **THEN** the confirmation shows the chat name, the network `whatsapp`, the text and the digest, and nothing is sent until `y`

#### Scenario: Recipients changed
- **WHEN** the daemon answers `recipients_changed`
- **THEN** the TUI reloads the preview and sends nothing

### Requirement: Beeper needs-check rows
A Beeper proposal in `executing` SHALL appear under needs-check with the outcome label only (for example `outcome_unknown`), never the message text, and SHALL offer only "mark sent" and "return to approved", each behind a confirmation.

#### Scenario: Needs-check row
- **WHEN** a Beeper send is ambiguous
- **THEN** the row shows the source, the label `outcome_unknown` and no text, and offers only the two human resolutions

### Requirement: Permission state for Beeper in the status view
The connector status view SHALL show, for each Beeper source, whether the kill switch is engaged, and the permission state "Beeper Desktop not running" or "Beeper token rejected" when the connector is in the `Authorization` state, with the command that fixes it.

#### Scenario: Beeper not running
- **WHEN** the WhatsApp connector last ended with `Authorization` for not running
- **THEN** the status view shows "Beeper Desktop not running" and no circuit-breaker notice
