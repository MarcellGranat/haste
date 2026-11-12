## ADDED Requirements

### Requirement: Agenda of upcoming events by day
The Calendar tab SHALL show the user's upcoming calendar events as an agenda grouped by day, soonest first. The window SHALL be provided by the daemon.

#### Scenario: Grouped by day
- **WHEN** there are two events today and one event tomorrow
- **THEN** the agenda shows a today heading with two events and a tomorrow heading with one event

#### Scenario: Window from daemon
- **WHEN** the daemon returns events for the agenda window
- **THEN** the TUI displays exactly those events and does not request events outside the window

### Requirement: Read-only agenda
The Calendar tab SHALL NOT create, update or delete calendar events.

#### Scenario: No write keys
- **WHEN** the user presses a key that would create an event in another tab
- **THEN** no event write request is issued from the Calendar tab

### Requirement: Calendar updates from daemon events
The agenda SHALL update from daemon change events without a manual refresh.

#### Scenario: Event added upstream
- **WHEN** the calendar connector syncs a new event while the Calendar tab is open
- **THEN** the event appears in the agenda without user action

### Requirement: Event text stays out of logs
The TUI SHALL NOT log event titles, attendees or descriptions.

#### Scenario: Log content
- **WHEN** the agenda is loaded
- **THEN** the log records only counts
