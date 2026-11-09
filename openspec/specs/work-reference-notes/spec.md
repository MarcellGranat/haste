# work-reference-notes Specification

## Purpose
Define identifier-only back-reference notes that link work items (threads, reminders, events) into the notes folder without copying content.

## Requirements

### Requirement: Identifier-only back-reference notes
An agent SHALL be able to record a back-reference to a thread, a reminder, or an event. The request SHALL carry a typed reference (`kind` one of `thread`, `reminder`, `event`, and an `id`), and the daemon SHALL append exactly one line `- haste:<kind>/<id>` under a dated heading. The request SHALL NOT carry free text.

#### Scenario: Thread reference
- **WHEN** an agent records `thread` reference `t-42` into `tasks/followups.md`
- **THEN** the file gains a dated heading and the line `- haste:thread/t-42`, and nothing else from the thread is written

#### Scenario: Free text is not accepted here
- **WHEN** a reference request includes a text field
- **THEN** the field is rejected as unknown and nothing is written

### Requirement: References are validated against the store
The daemon SHALL check that the identifier matches the store's identifier charset (ASCII, bounded length) and that the referenced object exists before writing. A reference to a missing object SHALL be refused with `NotFound`.

#### Scenario: Missing reminder
- **WHEN** an agent records `reminder` reference `r-missing`
- **THEN** the request fails with `NotFound` and nothing is written

#### Scenario: Malformed identifier
- **WHEN** an agent records a reference whose id contains a space or a newline
- **THEN** the request fails with `Validation` and nothing is written

### Requirement: No subjects, bodies, or addresses in references
Back-reference notes SHALL NOT contain email subjects, message bodies, reminder titles or notes, calendar titles, descriptions, or attendee addresses. The daemon SHALL compose these lines itself from the typed reference only.

#### Scenario: Reminder title is not copied
- **WHEN** a reference to a reminder titled "Call the bank" is recorded
- **THEN** the note contains `haste:reminder/<id>` and not the title

### Requirement: Event references follow the calendar change
The `event` kind SHALL be accepted only when the calendar event model is present in the running build. Otherwise the request SHALL be refused with `Validation` and reason `unknown_kind`.

#### Scenario: Event kind before calendar support
- **WHEN** an agent records an `event` reference in a build without the calendar event model
- **THEN** the request fails with `unknown_kind` and nothing is written

### Requirement: Reference notes live in fixed folders
Reference notes SHALL be written only under `tasks/` or under `projects/<project>/tasks/` with a known project label. The slug of the note name SHALL come from a human-supplied topic label, never from an email subject or reminder title.

#### Scenario: Topic label slug
- **WHEN** an agent records a thread reference with topic label "Billing follow-up"
- **THEN** the note is `tasks/billing-follow-up.md`
