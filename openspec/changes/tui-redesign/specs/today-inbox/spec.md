## ADDED Requirements

### Requirement: Unified inbox across channels
The Today tab SHALL list threads from all channels (mail and chats) in one list, restricted by the active preset filter. The list SHALL exclude archived threads and threads snoozed past the current time.

#### Scenario: Mixed sources in one list
- **WHEN** a Gmail thread and a WhatsApp thread are both unarchived and not snoozed
- **THEN** both appear in the Today list ordered by most recent activity

#### Scenario: Archived hidden
- **WHEN** a thread has `archived = true`
- **THEN** it does not appear in the Today list

### Requirement: Default filter shows all non-archived threads
When no `filters.json` exists, or no filter is marked active, the Today list SHALL show all non-archived threads.

#### Scenario: No filter file
- **WHEN** the haste data folder contains no `filters.json`
- **THEN** the Today list shows all non-archived threads

### Requirement: TODOs from Apple Reminders
The Today tab SHALL show a TODO section listing Apple Reminders that are due today or have no due date. Reminders due on any other day SHALL NOT appear.

#### Scenario: Due today and undated shown
- **WHEN** Reminders contain one item due today, one undated, and one due tomorrow
- **THEN** the TODO section shows the first two and not the third

#### Scenario: Completed reminders hidden
- **WHEN** a reminder due today is completed
- **THEN** it does not appear in the TODO section

### Requirement: Inbox updates from daemon events
The Today list SHALL update from daemon change events without a manual refresh.

#### Scenario: New message arrives
- **WHEN** a connector syncs a new unread thread while the Today tab is open
- **THEN** the thread appears in the list without user action

### Requirement: Message text stays out of logs
The TUI SHALL NOT write subject, sender, body or snippet text to any log.

#### Scenario: Log content
- **WHEN** a thread is opened and triaged from Today
- **THEN** the log records only the event kind and the thread identifier
