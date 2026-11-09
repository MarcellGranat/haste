## Why

Conversations the user has in iMessage and events on their Google Calendar sit outside the hub, so an agent working in a herdr workspace cannot see who the user is talking to or when they are free, and cannot propose a meeting. Both sources are reachable from the machine that runs the daemon: `chat.db` is local to macOS, and Google Calendar uses the same Google account and OAuth client that the Gmail connector already uses. This change adds a read-only iMessage connector and a Google Calendar connector, and lets agents propose calendar changes that a human approves. Nothing is sent automatically.

## What Changes

- Add an iMessage connector that reads the local Messages database `~/Library/Messages/chat.db` read-only. Each chat becomes a `Thread` with `source = imessage`; each message becomes a `Message`. Sync is incremental by `message.ROWID`. Requires Full Disk Access for the process that runs the daemon.
- The iMessage connector declares `read = true` and `draft = true` only. A draft is a stored proposal text that a human copies or dismisses. No code path sends an iMessage in this change, and `send` stays false.
- Add a Google Calendar connector that reads events from the human's Google calendars into the unified model as a new `Event` record with `source = gcal`. Sync is incremental using the Calendar `syncToken`.
- Google Calendar uses a separate consent with its own Keychain item `com.haste.gcal`, but reuses the Gmail OAuth client, PKCE loopback flow and token-store code. Gmail tokens and scopes are unchanged. Consent requests the minimal calendar scope needed for the approved-write path (see design).
- Agents may propose calendar events to create or change (`CalendarCreate`, `CalendarUpdate` proposals). A human approves each one in the TUI or CLI before the event is written. Writes are sent with attendee notifications disabled, so no invitation is emailed by the hub. There is no delete and no send-invite action.
- Both connectors are installed with their kill switch engaged. The human releases each one after authorizing it.
- Project tags for iMessage threads and calendar events come from herdr workspace labels using the existing scope mechanism; the human sets them. Machine principals see only items tagged for their own machine.
- Per-cycle audit entries contain counts and the source name only: no message text, subjects, attendee addresses, or event titles.
- Scope of the calendar: Google Calendar only. Apple Calendar (EventKit) is explicitly out of scope.
- **Human spike group first**: Full Disk Access on this macOS, the `chat.db` schema on this macOS version, and the Google Calendar scope consent screen are verified by the human before implementation begins (see tasks).

**BREAKING**: none. New sources and record types are additive.

## Capabilities

### New Capabilities
- `connector-imessage`: read-only `chat.db` sync into threads and messages, Full Disk Access handling, draft-only proposals, kill switch on first install, per-cycle counts audit.
- `connector-google-calendar`: Google Calendar OAuth consent reusing the Gmail client and token store with a separate Keychain item, incremental event read into the `Event` model, and approved-only create and update writes with no invitations sent and no deletes.

### Modified Capabilities
- `connector-gmail`: the OAuth client and token-store code are shared with the calendar connector. Gmail scopes and the `com.haste.gmail` Keychain item do not change. (Requirement-level change: the shared code path is parameterised by scope set and Keychain service.)
- `data-model`: adds the `Event` record, the `gcal` and `imessage` source values for events and threads, and event attendee participants mapped to `Person`.
- `proposals`: adds `CalendarCreate` and `CalendarUpdate` payloads, the iMessage `Draft` proposal that is never executed, and execution on approval only.
- `policy-gate`: adds the `calendar.create` and `calendar.update` actions, pending for agents and never automatic; `calendar.delete` and any `imessage.send` are not defined.
- `connector-framework`: connector descriptors may register with the kill switch engaged by default, used by both new connectors.

## Impact

- Code: `crates/haste-core` (model `Event`, `Source` variants, connector capability flags, policy actions), `crates/haste-connector-imessage` (new: chat.db reader, mapper), `crates/haste-connector-gcal` (new: Calendar client, mapper, fake-server tests), `crates/haste-connector-gmail` (parameterise shared OAuth/token code), `crates/hasted` (scheduler, default-engaged kill switch for new connectors, dispatch of approved calendar writes), `crates/haste` (human-only `connector gcal auth`, `connector status` lines), `crates/haste-tui` (calendar proposal rendering, iMessage draft view), `crates/haste-store` (event table, calendar sync cursor, scope tags for events), `crates/haste-proto`.
- Systems: macOS only for the iMessage connector (Full Disk Access on the daemon's binary). Google Calendar requires network access to `www.googleapis.com` and a Google Cloud OAuth client that already exists for Gmail.
- Dependencies: a SQLite driver already used by the store (read-only open); no new HTTP stack beyond the one Gmail uses. Final choices are in `design.md`.
- Coordination: `reminders-and-links` (in progress) also changes `connector-framework` capability flags, `proposals` and `policy-gate`. Implementation of this change must rebase on that flag set so the calendar and reminder flags do not collide.
- Tests: fakes only. A fake SQLite `chat.db` with the observed schema, and a local fake Google Calendar HTTP server in the style of the Gmail connector tests. No test reads a real Messages database or calls Google.
- Docs: README section for the iMessage Full Disk Access step and the Google Calendar auth step.
