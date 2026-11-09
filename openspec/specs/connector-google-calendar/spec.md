# connector-google-calendar Specification

## Purpose
Google Calendar connector: read events and write only human-approved event creates and updates, never delete, cancel or send invitations.

## Requirements

### Requirement: Google Calendar connector declares read and write for approved proposals only
The Google Calendar connector SHALL declare the capabilities `read = true`, `draft = false`, `send = false` and `archive = false`, plus the calendar write flag used only for approved `CalendarCreate` and `CalendarUpdate` proposals. It SHALL NOT expose a delete method, an invitation-send method, or any method that sets an event to cancelled.

#### Scenario: Capability declaration
- **WHEN** the daemon registers the Google Calendar connector
- **THEN** the declared capabilities are `read` and the calendar write flag, and no others

#### Scenario: Delete not available
- **WHEN** the connector and its guard are inspected for a way to delete, cancel or send an invitation for an event
- **THEN** none exists: the connector trait, the guard and the API client have no such method, the capability set has no such flag, and a test asserts that no HTTP DELETE is ever issued

### Requirement: Separate consent reusing the Gmail OAuth client
The calendar connector SHALL authorize with OAuth 2.0 using the same Google OAuth client, PKCE and loopback redirect on `127.0.0.1` as the Gmail connector. Its refresh token SHALL be stored only in the macOS Keychain under service `com.haste.gcal`; the OAuth client id and secret are kept in the same service under a separate account, never in `gcal.json`. The Gmail refresh token and the `com.haste.gmail` item SHALL NOT be read, changed, or revoked by calendar authorization. The requested scopes SHALL be the minimal set confirmed by the spike and SHALL NOT include `gmail` scopes.

#### Scenario: Consent creates a separate token
- **WHEN** the human runs `haste connector gcal auth` and approves consent
- **THEN** the refresh token is stored under `com.haste.gcal` and the `com.haste.gmail` item is unchanged

#### Scenario: Agent build has no auth verb
- **WHEN** an agent runs the `haste` CLI built without the human feature
- **THEN** no calendar auth verb exists

#### Scenario: Loopback only
- **WHEN** the consent flow runs
- **THEN** the redirect listener binds to `127.0.0.1` only and closes after one response or on timeout

### Requirement: Calendar token refusal opens the breaker
When Google rejects the calendar refresh token with invalid-grant or a restriction response, the connector SHALL return `Checkpoint`, the guard SHALL engage the `gcal` kill switch, no retry SHALL occur, and the human SHALL be notified that calendar re-authorization is needed.

#### Scenario: Invalid grant
- **WHEN** a calendar token refresh fails with `invalid_grant`
- **THEN** the `gcal` kill switch is engaged, a `breaker_opened` audit entry is written, and no retry occurs

### Requirement: Human selects calendars
The connector SHALL read only the calendars listed in the non-secret `gcal.json` settings file in the data directory, written with mode `0600`. That file SHALL NOT contain any token or secret.

#### Scenario: Unlisted calendar is not read
- **WHEN** a sync cycle runs and a calendar is not listed in `gcal.json`
- **THEN** no request is made for that calendar's events

### Requirement: Incremental read with sync tokens
The first cycle for each selected calendar SHALL list its events without a time window (a windowed listing returns no sync token), page to the end, store only events inside a bounded window of past and future days (both compile-time constants), and SHALL store the returned sync token. Later cycles SHALL request changes using the stored sync token. When Google reports the token as gone, the connector SHALL reset that calendar to a fresh full listing once and SHALL NOT loop.

#### Scenario: First cycle bounded
- **WHEN** a selected calendar is synced for the first time
- **THEN** the calendar is listed to the end without a time window, only events within the configured window are stored, and the sync token is stored

#### Scenario: Delta cycle
- **WHEN** a stored sync token exists
- **THEN** only changed and cancelled events since the token are returned and mapped

#### Scenario: Expired sync token
- **WHEN** Google reports the sync token as gone
- **THEN** that calendar is relisted once, the other calendars keep their tokens, and no retry loop occurs

### Requirement: Mapping events into the Event record
Each event SHALL map to one `Event` with `source = gcal` and `external_id` equal to `<calendarId>/<eventId>`. Attendees SHALL map to `Person` records. Description and location text SHALL be stored only in the local store. Event sync SHALL NOT overwrite local scope tags or triage state.

#### Scenario: Re-sync keeps local tags
- **WHEN** an event that already has human scope tags changes upstream
- **THEN** the event's title and times are updated and its scope tags are unchanged

#### Scenario: Description not in audit
- **WHEN** an event with a description is synced
- **THEN** no audit entry or log line contains the description text

### Requirement: Calendar creates and updates execute only on approval
A `CalendarCreate` or `CalendarUpdate` proposal SHALL be executed only after a human approves it. Execution SHALL use an insert or patch call with `sendUpdates` set to `none`. The hub SHALL NOT request `sendUpdates=all` or any value that emails attendees.

#### Scenario: Approved create
- **WHEN** the human approves a `CalendarCreate` proposal and then runs it (the explicit execute action, as for reminders)
- **THEN** one insert request is made with `sendUpdates=none`, the new event ID is recorded on the proposal, and an audit entry `calendar.create` with source `gcal` is written

#### Scenario: Pending or rejected creates nothing
- **WHEN** a calendar proposal is pending or rejected
- **THEN** no request is made to the Google Calendar API

#### Scenario: No invitation emails
- **WHEN** any calendar write is executed
- **THEN** the request carries `sendUpdates=none`

### Requirement: Update field allow-list and no attendee or cancel changes
`CalendarUpdate` SHALL accept only the fields title, start, end, description and location. It SHALL refuse a proposal that changes attendees, changes status to cancelled, or names any other field.

#### Scenario: Attendee change refused
- **WHEN** a `CalendarUpdate` proposal includes an attendee change
- **THEN** the proposal is rejected with a validation error and no request is made

#### Scenario: Cancel refused
- **WHEN** a `CalendarUpdate` proposal sets status to cancelled
- **THEN** the proposal is rejected and no request is made

### Requirement: Stale event blocks approval
When the source event changed after a `CalendarUpdate` proposal was created, the proposal SHALL be flagged stale and approval SHALL require the existing stale confirmation.

#### Scenario: Stale update
- **WHEN** a human approves an update whose source event changed after proposal creation
- **THEN** the stale confirmation is required before the patch is sent

### Requirement: Kill switch engaged on first install
The calendar connector SHALL register with its kill switch engaged when no persisted state exists. Scheduled reads and approved writes SHALL be refused until the human releases the connector.

#### Scenario: Fresh install
- **WHEN** the daemon starts for the first time with the calendar connector registered
- **THEN** the `gcal` kill switch is engaged and no request is made until `haste connector release gcal`

### Requirement: Per-cycle audit with counts only
Every calendar sync cycle SHALL write one audit entry containing the source `gcal`, the outcome, counts of new and changed events and any cursor reset. Audit entries, logs and errors SHALL NOT contain event titles, descriptions, locations, or attendee addresses.

#### Scenario: Cycle audit
- **WHEN** a calendar sync cycle finishes
- **THEN** one audit entry with source `gcal` and counts is appended, with no title, description or attendee text
