## ADDED Requirements

### Requirement: Archive key
Pressing `e` on a selected thread SHALL archive it. For mail threads the archive SHALL be applied in Gmail or iCloud immediately as a human-only action. For chat threads the archive SHALL be local to haste.

#### Scenario: Mail archived upstream
- **WHEN** the user presses `e` on a Gmail thread
- **THEN** the daemon archives the thread in Gmail and then sets `archived = true` locally

#### Scenario: Chat archived locally
- **WHEN** the user presses `e` on a WhatsApp thread
- **THEN** only `archived = true` is set in haste and no upstream request is made

#### Scenario: Agent cannot archive
- **WHEN** an agent principal requests an archive of any thread
- **THEN** the daemon refuses the request

### Requirement: Reply key opens a human draft only
Pressing `r` on a selected thread SHALL open a reply draft for the human. When the thread has more than one distinct recipient, the TUI SHALL first ask whether to reply or reply-all. The key SHALL NOT send a message.

#### Scenario: Single recipient
- **WHEN** the user presses `r` on a thread with one sender
- **THEN** a reply draft opens without a reply-or-reply-all prompt

#### Scenario: Several recipients
- **WHEN** the user presses `r` on a thread with three distinct recipients
- **THEN** the TUI asks reply or reply-all before opening the draft

#### Scenario: No send
- **WHEN** the user presses `r` and leaves the draft unsent
- **THEN** no send request is issued to the daemon

### Requirement: Put aside, priority and snooze keys
Pressing `g` SHALL toggle `set_aside` on the thread. Pressing `p` SHALL toggle `priority`. Pressing `s` SHALL ask for a duration in hours or days and SHALL set `snoozed_until` to now plus that duration.

#### Scenario: Snooze for two days
- **WHEN** the user presses `s` and enters `2 days`
- **THEN** `snoozed_until` is set to two days from now and the thread leaves the Today list

#### Scenario: Aside toggle
- **WHEN** the user presses `g` on a thread with `set_aside = false`
- **THEN** `set_aside` becomes true

### Requirement: Reminder key links a reminder to the thread
Pressing `t` on a selected thread SHALL create a reminder linked to the thread through the existing reminder-link path, and SHALL NOT use the `r` key for this action.

#### Scenario: Reminder for thread
- **WHEN** the user presses `t` on a thread
- **THEN** a reminder linked to that thread is created through the proposal path

### Requirement: Triage writes preserve connector data
Triage writes from these keys SHALL NOT modify thread or message content, and connector re-syncs SHALL NOT reset triage fields.

#### Scenario: Re-sync after aside
- **WHEN** a thread is set aside and then re-synced by its connector
- **THEN** `set_aside` remains true

### Requirement: All triage effects go through the daemon
Every key in this capability SHALL issue a daemon request. The TUI SHALL NOT write the store or call a connector directly.

#### Scenario: Snooze request
- **WHEN** the user snoozes a thread
- **THEN** the TUI sends a snooze request to the daemon and does not open the store
