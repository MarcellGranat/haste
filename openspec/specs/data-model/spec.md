# data-model Specification

## Purpose
TBD - created by syncing change core-policy-gate. Update Purpose after archive.

## Requirements

### Requirement: Thread carries its source
Every `Thread` SHALL have an identifier, a `source` value from the closed set of known sources plus an explicit `other` variant carrying a name, a `external_id` unique within its source, a subject, a participant list, and a `last_activity_at` timestamp. A thread SHALL carry a monotonically increasing `version` used for stale-thread detection.

#### Scenario: Thread version increments on change
- **WHEN** a thread's content or participants change through a connector sync
- **THEN** its `version` increases by exactly one

#### Scenario: Duplicate external identifier within a source
- **WHEN** a sync inserts a thread whose `(source, external_id)` already exists
- **THEN** the existing thread is updated and no second row is created

### Requirement: Message belongs to a thread
Every `Message` SHALL reference exactly one thread, SHALL reference a sender `Person` when one is known, and SHALL store its body as text. Messages SHALL be unique on `(source, external_id)`.

#### Scenario: Message without known sender
- **WHEN** a message is synced whose sender identifier matches no `Person`
- **THEN** the message is stored with no sender person and the sender identifier is preserved

### Requirement: Person has identifiers
A `Person` SHALL have a display name and a list of typed identifiers (for example email address, phone number, chat handle). An identifier SHALL belong to at most one person.

#### Scenario: Identifier conflict
- **WHEN** an attempt is made to attach an identifier already owned by another person
- **THEN** the operation is rejected and no change is made

### Requirement: Local triage state is separate from synced data
Triage state SHALL consist of `priority`, `snoozed_until`, `set_aside`, and `archived`, and SHALL be stored separately from thread and message content. Connector re-syncs MUST NOT overwrite triage state.

#### Scenario: Re-sync preserves triage
- **WHEN** a connector re-syncs a thread that has `set_aside = true`
- **THEN** the thread's `set_aside` value remains true after the sync

#### Scenario: Snooze sets timestamp
- **WHEN** a snooze is applied with a target time
- **THEN** `snoozed_until` equals that time and the thread is hidden from default views until that time

### Requirement: Typed links between objects
The system SHALL store links between objects with a `from` kind and id, a `to` kind and id, and a `link_type` drawn from a closed vocabulary. Links MUST be validated against the vocabulary on insert, and an unknown link type SHALL be rejected.

#### Scenario: Link with unknown type
- **WHEN** a client creates a link whose `link_type` is not in the vocabulary
- **THEN** the daemon rejects the request with a validation error

#### Scenario: Link to nonexistent object
- **WHEN** a client creates a link referencing an object id that does not exist
- **THEN** the daemon rejects the request and stores no link

### Requirement: Unified inbox query supports per-source filter
The data model SHALL support listing threads across all sources, and SHALL support filtering by one or more `source` values. Archived and snoozed threads SHALL be excluded from the default listing.

#### Scenario: Filter by source
- **WHEN** a listing is requested with `source = email`
- **THEN** only threads whose source is `email` are returned

#### Scenario: Default listing hides archived
- **WHEN** a listing is requested without an archived flag
- **THEN** threads with `archived = true` are not returned

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
