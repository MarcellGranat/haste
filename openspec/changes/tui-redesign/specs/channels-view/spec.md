## ADDED Requirements

### Requirement: Channels grouped by source
The Channels tab SHALL group the unified inbox by `source` and SHALL show the channels gmail, icloud, whatsapp, messenger and imessage, each with a count of non-archived threads.

#### Scenario: Group counts
- **WHEN** there are four unread Gmail threads and two WhatsApp threads
- **THEN** the Channels tab shows gmail with count 4 and whatsapp with count 2

#### Scenario: Empty channel shown
- **WHEN** there are no iMessage threads
- **THEN** imessage is shown with count 0

### Requirement: Per-channel list
Selecting a channel SHALL show the threads of that channel using the same row rendering and triage keys as the Today tab.

#### Scenario: Open channel
- **WHEN** the user selects the whatsapp channel and presses Enter
- **THEN** the list shows only WhatsApp threads

### Requirement: Channels respect the active filter
The per-channel counts and lists SHALL respect the active preset filter.

#### Scenario: Filter applied
- **WHEN** the active filter has `unread = true`
- **THEN** each channel count includes only unread threads
