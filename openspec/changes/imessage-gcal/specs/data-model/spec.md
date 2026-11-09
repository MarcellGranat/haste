## ADDED Requirements

### Requirement: Event record for calendar events
The data model SHALL include an `Event` record with fields `source`, `external_id`, `calendar`, `title`, `start`, `end`, `all_day`, `status`, `attendees`, `location_present` and `updated_at`. An `Event` SHALL carry `source = gcal` for Google Calendar events and its scope tags and triage state SHALL be stored separately from synced fields.

#### Scenario: Event carries its source
- **WHEN** a Google Calendar event is stored
- **THEN** its `source` is `gcal` and its `external_id` is `<calendarId>/<eventId>`

#### Scenario: Local fields survive sync
- **WHEN** a synced event is updated
- **THEN** its scope tags and triage state are unchanged

### Requirement: iMessage threads and messages use the existing model
iMessage chats SHALL be stored as `Thread` with `source = imessage` and messages as `Message` in that thread, without a new record type.

#### Scenario: iMessage chat stored as thread
- **WHEN** an iMessage chat is synced
- **THEN** it is stored as a `Thread` with `source = imessage`

### Requirement: Event links use the typed link vocabulary
Links between an `Event` and a `Thread` or `Message` SHALL use the existing closed link vocabulary and SHALL NOT add a new link kind in this change.

#### Scenario: Link to thread
- **WHEN** a human links an event to a thread
- **THEN** the link kind is taken from the existing closed vocabulary
