## Why

Events on the user's Google Calendar sit outside the hub, so an agent working in a herdr workspace cannot see when the user is free and cannot propose a meeting. Google Calendar uses the same Google account and OAuth client that the Gmail connector already uses. This change adds a Google Calendar connector and lets agents propose calendar changes that a human approves. Nothing is sent automatically.

## What Changes

- Add a Google Calendar connector that reads events from the human's Google calendars into the unified model as a new `Event` record with `source = gcal`. Sync is incremental using the Calendar `syncToken`.
- Google Calendar uses a separate consent with its own Keychain item `com.haste.gcal`, but reuses the Gmail OAuth client, PKCE loopback flow and token-store code. Gmail tokens and scopes are unchanged. Consent requests `calendar.events` and `calendar.calendarlist.readonly` (confirmed by the spike, see design).
- The connector reads only the calendars listed in a non-secret `gcal.json` config that the human fills in; which calendars to sync is not a blocker for implementation.
- Agents may propose calendar events to create or change (`CalendarCreate`, `CalendarUpdate` proposals). A human approves each one in the TUI or CLI before the event is written. Writes are sent with attendee notifications disabled, so no invitation is emailed by the hub. There is no delete and no send-invite action.
- The connector is installed with its kill switch engaged. The human releases it after authorizing it.
- Project tags for calendar events come from herdr workspace labels using the existing scope mechanism; the human sets them. Machine principals see only items tagged for their own machine.
- Per-cycle audit entries contain counts and the source name only: no subjects, attendee addresses, or event titles.
- Scope of the calendar: Google Calendar only. Apple Calendar (EventKit) is explicitly out of scope.
- **Human spike first**: the Google Calendar scope consent screen is verified by the human before implementation begins (done, see tasks).

The iMessage connector was split out into the separate, parked change `imessage-connector`.

**BREAKING**: none. The new source and record type are additive.

## Capabilities

### New Capabilities
- `connector-google-calendar`: Google Calendar OAuth consent reusing the Gmail client and token store with a separate Keychain item, incremental event read into the `Event` model, and approved-only create and update writes with no invitations sent and no deletes.

### Modified Capabilities
- `connector-gmail`: the OAuth client and token-store code are shared with the calendar connector. Gmail scopes and the `com.haste.gmail` Keychain item do not change. (Requirement-level change: the shared code path is parameterised by scope set and Keychain service.)
- `data-model`: adds the `Event` record, the `gcal` source value for events, and event attendee participants mapped to `Person`.
- `proposals`: adds `CalendarCreate` and `CalendarUpdate` payloads and execution on approval only.
- `policy-gate`: adds the `calendar.create` and `calendar.update` actions, pending for agents and never automatic; `calendar.delete` is not defined.
- `connector-framework`: connector descriptors may register with the kill switch engaged by default, used by the calendar connector (and later the iMessage connector).

## Impact

- Code: `crates/haste-core` (model `Event`, `Source` variant, connector capability flags, policy actions), `crates/haste-connector-gcal` (new: Calendar client, mapper, fake-server tests), `crates/haste-connector-gmail` (parameterise shared OAuth/token code), `crates/hasted` (scheduler, default-engaged kill switch, dispatch of approved calendar writes), `crates/haste` (human-only `connector gcal auth`, `connector status` line), `crates/haste-tui` (calendar proposal rendering), `crates/haste-store` (event table, calendar sync cursor, scope tags for events), `crates/haste-proto`.
- Systems: Google Calendar requires network access to `www.googleapis.com` and a Google Cloud OAuth client that already exists for Gmail. The app stays in Testing status (no homepage or privacy policy URL), so refresh tokens expire after 7 days and the revoked-token path covers re-authorization.
- Dependencies: no new HTTP stack beyond the one Gmail uses. Final choices are in `design.md`.
- Coordination: `reminders-and-links` (in progress) also changes `connector-framework` capability flags, `proposals` and `policy-gate`. Implementation of this change must rebase on that flag set so the calendar and reminder flags do not collide.
- Tests: fakes only: a local fake Google Calendar HTTP server in the style of the Gmail connector tests. No test calls Google.
- Docs: README section for the Google Calendar auth step.
