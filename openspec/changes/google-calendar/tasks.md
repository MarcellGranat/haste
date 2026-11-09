## 1. Human spike (upfront, before any code)

- [x] 1.1 Run the Google Calendar consent once against the existing OAuth client with the candidate scopes `calendar.events` and `calendar.calendarlist.readonly`; confirm the consent screen, that `calendarList.list` works with the scopes, and whether `calendar.calendarlist.readonly` can be dropped (it cannot); update the connector-google-calendar scope list accordingly
- [x] 1.2 Human sign-off on the spike findings before section 2 starts

## 2. Shared Google OAuth refactor

- [x] 2.1 Extract the Gmail OAuth consent, PKCE, loopback listener and token-refresh code into a shared module parameterised by scope set and Keychain service
- [x] 2.2 Switch the Gmail connector to the shared module with its existing parameters (`gmail.readonly`, `gmail.compose`, `com.haste.gmail`)
- [x] 2.3 Run the existing Gmail connector test suite unchanged and confirm it passes (connector-gmail ADDED requirement)

## 3. Core model and policy

- [x] 3.1 Add `Source::GoogleCalendar` (name `gcal`) in `haste-core`
- [x] 3.2 Add the `Event` record and its store table with local-field separation for scope tags and triage state
- [ ] 3.3 Add `CalendarCreate` and `CalendarUpdate` proposal payloads, with the `CalendarUpdate` field allow-list enforced at creation
- [x] 3.4 Add `calendar.create` and `calendar.update` to the policy gate as `pending` for agents; confirm `calendar.delete` resolves to `denied` as an undefined action
- [ ] 3.5 Add the default-engaged flag to the connector descriptor and a test that a fresh store starts the flagged connector engaged
- [x] 3.6 Coordinate with the `reminders-and-links` capability-flag and proposal-kind set before adding flags, and rebase if that change has landed

## 4. Google Calendar connector

- [x] 4.1 Create `crates/haste-connector-gcal` with the `Connector` implementation, read capability and the calendar write flag, and no delete or cancel method
- [ ] 4.2 Implement the `gcal` consent verb in the human build, storing the token under `com.haste.gcal` and leaving the Gmail item untouched
- [x] 4.3 Read the non-secret `gcal.json` calendar list (mode `0600`) and refuse unlisted calendars; a missing or empty list reads nothing
- [x] 4.4 Implement the unwindowed first listing storing only in-window events, and `syncToken` delta sync, with a single bounded reset on `410 Gone` and no loop
- [x] 4.5 Implement the event to `Event` mapping, attendee to person mapping, and the local-field preservation on re-sync
- [ ] 4.6 Implement `CalendarCreate` execution with `events.insert` and `sendUpdates=none`, and `CalendarUpdate` execution with `events.patch` limited to the allow-list and refusing attendee or cancel changes
- [ ] 4.7 Implement the stale-event check on update approval, reusing the existing stale confirmation
- [x] 4.8 Map invalid-grant and restriction responses to `Checkpoint` with the breaker and re-authorization notice
- [ ] 4.9 Implement the per-cycle counts-only audit entry and `calendar.create` / `calendar.update` audit entries with the event ID as detail
- [ ] 4.10 Register the connector with default-engaged kill switch and the `haste connector release|engage|status gcal` verbs
- [x] 4.11 Build the local fake Google Calendar server in the style of the Gmail test server, serving calendarList, events.list with syncToken (and a windowed listing without one), a 410 response, events.insert and events.patch
- [ ] 4.12 Add tests for: sendUpdates=none on every write, no delete route, token isolation from the Gmail item, syncToken delta, unwindowed first listing with in-window storage, 410 single reset, allow-list refusal, attendee and cancel refusal, stale confirmation, invalid-grant breaker, and audit content

## 5. Daemon, scope and TUI

- [ ] 5.1 Wire the connector into the scheduler through `Guarded` with its own interval and kill switch
- [ ] 5.2 Dispatch approved calendar proposals through the connector only after approval, with the proposal recording the result
- [ ] 5.3 Extend the exhaustive per-principal scope-filter test to `Event`
- [ ] 5.4 Render calendar proposals (title, calendar, time, project) in the approvals view, and show the default-engaged state in the connectors status
- [ ] 5.5 Add the `haste connector status` line for `gcal` in the human build and confirm the agent build does not list the auth verb

## 6. Docs and checks

- [ ] 6.1 Add a README section for the Google Calendar consent step and `gcal.json`, with the human-only verbs stated
- [ ] 6.2 Run `cargo test` for the workspace and the new crate's tests, with no test calling Google
- [ ] 6.3 Run `cargo clippy` with warnings as errors on the changed crates and `cargo fmt --check`

## 7. Manual smoke and config (end, human-run on this Mac, real accounts)

- [ ] 7.1 Fresh install: start the daemon and confirm `haste connector status` lists `gcal` as engaged, with no reads performed
- [ ] 7.2 Choose which of the 4 calendars to sync and write their IDs into `gcal.json` (mode `0600`, no secrets); decide which herdr workspace labels serve as project tags and set them on the synced events
- [ ] 7.3 Run `haste connector gcal auth`, confirm the consent screen shows only the calendar scopes, and confirm `com.haste.gmail` in Keychain is unchanged
- [ ] 7.4 Release `gcal` with one test calendar selected and confirm events appear and a second cycle uses the sync token
- [ ] 7.5 Have an agent propose a `CalendarCreate` on a throwaway test event, approve it in the TUI, and confirm the event exists in Google Calendar with no invitation email sent
- [ ] 7.6 Propose and approve a `CalendarUpdate` changing the title of that test event; confirm an attendee-change proposal is rejected
- [ ] 7.7 Engage `gcal` again and confirm all reads and approved writes are refused; delete the throwaway test event by hand in Google Calendar
