## ADDED Requirements

### Requirement: Reminder record
Every `Reminder` SHALL have an identifier, a `source` value of `reminders`, an `external_id` unique within its source (the EventKit `calendarItemIdentifier`), a `title`, a `notes_snippet` of bounded length, an optional `due_at` timestamp, a `completed` flag, a `list_name`, an optional `missing_since` timestamp, and a `version` used for stale-proposal detection. The `notes_snippet` SHALL NOT be written to any log.

#### Scenario: Duplicate external identifier
- **WHEN** a sync inserts a reminder whose `(source, external_id)` already exists
- **THEN** the existing reminder is updated and no second row is created

#### Scenario: Notes are bounded
- **WHEN** a reminder's notes exceed the snippet limit
- **THEN** only the bounded snippet is stored

### Requirement: Reminder re-sync preserves local fields
A Reminders re-sync SHALL update only the remote-owned fields (`title`, `notes_snippet`, `due_at`, `completed`, `list_name`, `missing_since`, `version`). It MUST NOT change reminder scope tags, triage state, or links.

#### Scenario: Re-sync keeps tags
- **WHEN** a reminder with scope tag `haste` is re-synced with a changed title
- **THEN** the title changes and the tag `haste` remains

#### Scenario: Re-sync keeps links
- **WHEN** a reminder linked to a thread is re-synced
- **THEN** the link remains unchanged

### Requirement: Reminder version increments on remote change
A reminder's `version` SHALL increase by exactly one when its remote-owned fields change during sync.

#### Scenario: Unchanged reminder
- **WHEN** a sync returns a reminder with identical remote-owned fields
- **THEN** its `version` is unchanged

### Requirement: Reminder kinds in typed links
The typed-link store SHALL accept reminders as link endpoints, with the same closed link vocabulary used for threads, messages and people.

#### Scenario: Link from reminder to thread
- **WHEN** a link of type `about` is created from a reminder to a thread
- **THEN** the link is stored after validating both endpoints exist
