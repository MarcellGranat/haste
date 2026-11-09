## 1. Human spike (upfront, before any code)

- [ ] 1.1 Grant Full Disk Access to the exact binary the daemon runs from on this Mac, and confirm a read-only open of `~/Library/Messages/chat.db` succeeds and fails without it; record the binary path and the observed error text
- [ ] 1.2 Record the `chat.db` schema on this macOS version (`message`, `chat`, `handle`, `chat_message_join` columns), the `message.date` unit, the share of rows with NULL `text`, and whether recent rows are visible without the `-wal`/`-shm` sidecars under read-only open; update design.md Open Questions with the answers
- [ ] 1.3 Run the Google Calendar consent once against the existing OAuth client with the candidate scopes `calendar.events` and `calendar.calendarlist.readonly`; confirm the consent screen, that `calendarList.list` works with the scopes, and whether `calendar.calendarlist.readonly` can be dropped; update the connector-google-calendar scope list accordingly
- [ ] 1.4 Confirm the human's calendar IDs and the herdr workspace labels that will serve as project tags, and record them in design.md (no secrets recorded)
- [ ] 1.5 Human sign-off on the spike findings before section 2 starts

## 2. Shared Google OAuth refactor

- [ ] 2.1 Extract the Gmail OAuth consent, PKCE, loopback listener and token-refresh code into a shared module parameterised by scope set and Keychain service
- [ ] 2.2 Switch the Gmail connector to the shared module with its existing parameters (`gmail.readonly`, `gmail.compose`, `com.haste.gmail`)
- [ ] 2.3 Run the existing Gmail connector test suite unchanged and confirm it passes (connector-gmail ADDED requirement)

## 3. Core model and policy

- [ ] 3.1 Add `Source::GoogleCalendar` (name `gcal`) and confirm `Source::Imessage` (name `imessage`) round-trips in `haste-core`
- [ ] 3.2 Add the `Event` record and its store table with local-field separation for scope tags and triage state
- [ ] 3.3 Add `CalendarCreate`, `CalendarUpdate` and iMessage `Draft` proposal payloads, with the `CalendarUpdate` field allow-list enforced at creation
- [ ] 3.4 Add `calendar.create` and `calendar.update` to the policy gate as `pending` for agents; confirm `calendar.delete` and `imessage.send` resolve to `denied` as undefined actions
- [ ] 3.5 Add the default-engaged flag to the connector descriptor and a test that a fresh store starts the flagged connector engaged
- [ ] 3.6 Coordinate with the `reminders-and-links` capability-flag and proposal-kind set before adding flags, and rebase if that change has landed

## 4. iMessage connector

- [ ] 4.1 Create `crates/haste-connector-imessage` with the `Connector` implementation, capabilities `read` and `draft` only, and no send method
- [ ] 4.2 Implement the read-only `chat.db` open, mapping `PermissionNeeded` on permission errors with no retry
- [ ] 4.3 Implement ROWID-cursor incremental sync with bounded first backfill and idempotent message upsert
- [ ] 4.4 Implement the chat to thread, message to message, handle to person, and Messages-epoch date mapping, with NULL-text rows marked body-unavailable
- [ ] 4.5 Implement the per-cycle counts-only audit entry
- [ ] 4.6 Register the connector with default-engaged kill switch and the `haste connector release|engage|status imessage` verbs in the human build
- [ ] 4.7 Implement the draft approve path that marks "approved for manual copy" with no transmission, and the TUI draft view
- [ ] 4.8 Build the fake `chat.db` test fixture with the observed schema, a NULL-text row, and a nanosecond-date row; add tests for read-only handle, ROWID delta, idempotency, PermissionNeeded, date conversion, and audit content
- [ ] 4.9 Add a test that the connector directory's mtimes and contents are unchanged after a sync

## 5. Google Calendar connector

- [ ] 5.1 Create `crates/haste-connector-gcal` with the `Connector` implementation, read capability and the calendar write flag, and no delete or cancel method
- [ ] 5.2 Implement the `gcal` consent verb in the human build, storing the token under `com.haste.gcal` and leaving the Gmail item untouched
- [ ] 5.3 Read the non-secret `gcal.json` calendar list with mode `0600` and refuse unlisted calendars
- [ ] 5.4 Implement the bounded first window and `syncToken` delta sync, with a single bounded reset on `410 Gone` and no loop
- [ ] 5.5 Implement the event to `Event` mapping, attendee to person mapping, and the local-field preservation on re-sync
- [ ] 5.6 Implement `CalendarCreate` execution with `events.insert` and `sendUpdates=none`, and `CalendarUpdate` execution with `events.patch` limited to the allow-list and refusing attendee or cancel changes
- [ ] 5.7 Implement the stale-event check on update approval, reusing the existing stale confirmation
- [ ] 5.8 Map invalid-grant and restriction responses to `Checkpoint` with the breaker and re-authorization notice
- [ ] 5.9 Implement the per-cycle counts-only audit entry and `calendar.create` / `calendar.update` audit entries with the event ID as detail
- [ ] 5.10 Register the connector with default-engaged kill switch and the `haste connector release|engage|status gcal` verbs
- [ ] 5.11 Build the local fake Google Calendar server in the style of the Gmail test server, serving calendarList, events.list with syncToken, a 410 response, events.insert and events.patch
- [ ] 5.12 Add tests for: sendUpdates=none on every write, no delete route, token isolation from the Gmail item, syncToken delta, 410 single reset, allow-list refusal, attendee and cancel refusal, stale confirmation, invalid-grant breaker, and audit content

## 6. Daemon, scope and TUI

- [ ] 6.1 Wire both connectors into the scheduler through `Guarded` with their own intervals and kill switches
- [ ] 6.2 Dispatch approved calendar proposals through the connector only after approval, with the proposal recording the result
- [ ] 6.3 Extend the exhaustive per-principal scope-filter test to `Event` and iMessage threads
- [ ] 6.4 Render calendar proposals (title, calendar, time, project) and iMessage drafts in the approvals view, and show the `PermissionNeeded` and default-engaged states in the connectors status
- [ ] 6.5 Add the `haste connector status` lines for `imessage` and `gcal` in the human build and confirm the agent build does not list the auth verbs

## 7. Docs and checks

- [ ] 7.1 Add README sections for the Full Disk Access step and the Google Calendar consent step, with the human-only verbs stated
- [ ] 7.2 Run `cargo test` for the workspace and the new crates' tests, with no test reading a real Messages database or calling Google
- [ ] 7.3 Run `cargo clippy` with warnings as errors on the changed crates and `cargo fmt --check`

## 8. Manual smoke (end, human-run on this Mac, real accounts)

- [ ] 8.1 Fresh install: start the daemon and confirm `haste connector status` lists `imessage` and `gcal` as engaged, with no reads performed
- [ ] 8.2 Without Full Disk Access, release `imessage` and confirm the status shows PermissionNeeded naming the binary, with no retry loop in the audit log
- [ ] 8.3 Grant Full Disk Access, release `imessage`, and confirm one cycle adds threads and messages with counts-only audit and no message text in the audit log
- [ ] 8.4 Confirm a sent iMessage (sent from the Messages app) appears on the next cycle, and that haste never transmits anything
- [ ] 8.5 Run `haste connector gcal auth`, confirm the consent screen shows only the calendar scopes, and confirm `com.haste.gmail` in Keychain is unchanged
- [ ] 8.6 Select one test calendar in `gcal.json`, release `gcal`, and confirm events appear and a second cycle uses the sync token
- [ ] 8.7 Have an agent propose a `CalendarCreate` on a throwaway test event, approve it in the TUI, and confirm the event exists in Google Calendar with no invitation email sent
- [ ] 8.8 Propose and approve a `CalendarUpdate` changing the title of that test event; confirm an attendee-change proposal is rejected
- [ ] 8.9 Propose an iMessage draft, approve it, and confirm it is marked approved for manual copy and nothing was sent
- [ ] 8.10 Engage `gcal` and `imessage` again and confirm all reads and approved writes are refused; delete the throwaway test event by hand in Google Calendar
