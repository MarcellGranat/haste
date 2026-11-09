## Context

The hub already has the pattern this change needs. Each connector implements the `Connector` trait, the daemon calls it only through `Guarded` (kill switch, capability flags, circuit breaker, bounded transient retry, audit), and the Gmail connector shows the OAuth + Keychain + fake-server test shape. `Source` already includes `Imessage` and `Gmail`; there is no calendar source or event record yet. The `reminders-and-links` change is in flight and touches the same capability flags and proposal kinds, so this design keeps its additions narrow.

Constraints from the request: iMessage replies are proposals only and are never sent; calendar means Google Calendar only; agents may propose calendar creates and updates, a human approves each one; no invitations are sent by the hub; no deletes; kill switch engaged on first install; per-cycle audit with counts only; project tags from herdr workspace labels; scope filter for machine principals; fakes-only tests.

## Goals / Non-Goals

**Goals:**
- Read iMessage chats and Google Calendar events into the unified model, incrementally and read-only.
- Let agents propose calendar creates and updates and let the human approve or reject them.
- Keep the blast radius of the calendar consent separate from Gmail.
- Make both connectors safe on first install: kill switch engaged until the human authorizes and releases.

**Non-Goals:**
- Sending iMessages or any draft that leaves the hub automatically. Drafts are text in a proposal only.
- Apple Calendar / EventKit. Calendar means Google Calendar.
- Deleting events, changing attendees' invitations, or sending invitation emails.
- Reading iMessage attachments, or remote machines' Messages databases.
- Changing Gmail scopes or the `com.haste.gmail` Keychain item.

## Decisions

### D1. iMessage reads `chat.db` directly, read-only, from a copy-free handle
Open `~/Library/Messages/chat.db` with `SQLITE_OPEN_READ_ONLY` through the store's existing SQLite driver. Never write to it, never create `-wal`/`-shm` files beside it if avoidable (open with `immutable=0` and `query_only` pragma; the spike decides whether WAL sidecars are readable under read-only open). Sync reads `message` joined to `chat_message_join` and `chat` with `handle` for addresses, filtered by `message.ROWID > cursor`. The cursor is the max ROWID seen, stored per connector as `Cursor`.
- Alternative: copy `chat.db` to a scratch file each cycle. Rejected: copies duplicate message content on disk and the copy can be stale; read-only open is enough.
- Dates: `message.date` is nanoseconds since 2001-01-01 on modern macOS; the mapper converts to Unix time and the spike confirms the divisor on this macOS.
- Body text: `message.text` may be NULL on newer macOS with the body only in `attributedBody` (typedstream). v1 stores `text` when present; for NULL-text rows it stores the body as unavailable rather than parsing typedstream. Parsing typedstream is a follow-up unless the spike shows most rows are NULL-text.

### D2. iMessage is read plus draft proposals only
The connector declares `read = true`, `draft = true`, `send = false`, `archive = false`. A draft is a `Draft` proposal whose body is text the human can copy. It is never dispatched anywhere. There is no `send` method on the connector, so `Guarded` refuses any send call with a capability-missing error. The TUI shows drafts with the target thread and a dismiss action; approval only marks the draft "approved for manual copy" and does not transmit it.

### D3. Full Disk Access is a human precondition, detected not requested
Without Full Disk Access, opening `chat.db` fails with a permission error. The connector maps that to a new `PermissionNeeded` outcome (treated like a checkpoint for the human, but not a challenge): the kill switch stays engaged, the status view says "Full Disk Access needed for the daemon binary", and nothing retries in a loop. The human grants FDA to the daemon's binary and releases the connector. The daemon never requests FDA itself.

### D4. Google Calendar reuses the Gmail OAuth client and PKCE flow, with a separate consent and Keychain item
The consent and token code in `haste-connector-gmail` is moved behind a small shared module (or the gcal crate depends on it) parameterised by `(scopes, keychain_service)`. The calendar connector uses the same Google Cloud OAuth client ID, PKCE, loopback redirect on `127.0.0.1`, and the same refresh-then-access-token flow, but stores its refresh token under `com.haste.gcal`.
- Alternative: incremental authorization that adds a calendar scope to the Gmail refresh token. Rejected: it widens the Gmail token, couples revocation of calendar and Gmail, and makes a Gmail-only checkpoint kill calendar too.
- Scopes: the spike confirms the minimal set. Working assumption: `calendar.events` (read and approved write of events) plus `calendar.calendarlist.readonly` (listing the human's calendars). If the spike shows `calendar.events` alone covers `calendarList.list`, drop the second scope. `calendar.readonly` is not requested because it cannot write.
- The consent screen shows the unverified-app warning for the existing production client; the human acknowledges it once, as in Gmail.

### D5. Calendar read is incremental with `syncToken`
Google only returns a `nextSyncToken` for a listing made without `timeMin`/`timeMax` (documented restriction on incremental sync; the spike saw no token on a windowed first page). So the first cycle lists each selected calendar **without a time window**, pages to the end to get the `nextSyncToken`, and **stores only events inside the window** (now minus `CAL_BACKFILL_DAYS`, now plus `CAL_LOOKAHEAD_DAYS`, both compile-time constants; recurring series are kept as one master event, not expanded). Events outside the window are discarded locally. Later cycles pass the token and receive changed and cancelled events. A `410 Gone` on the token resets that calendar to a fresh full listing once and does not loop. Only calendars the human lists in `gcal.json` (non-secret, mode `0600`) are read.

### D6. Calendar events map to a new `Event` record
`Event { source: gcal, external_id: "<calendarId>/<eventId>", calendar, title, start, end, all_day, location_present, status, attendees, updated_at }`. Attendees map to `Person` via the existing identifiers. Description and location text are stored only in the local store, never in audit or logs. Event sync never overwrites local fields (scope tags, triage state), matching the reminders rule.

### D7. Calendar writes are proposals executed only on approval
Two new proposal payloads: `CalendarCreate { calendar, title, start, end, attendees?, description? }` and `CalendarUpdate { event_ref, changed_fields }`. Both carry the herdr workspace name as project when an agent proposes them. Policy: `calendar.create` and `calendar.update` are `pending` for agents and never automatic. On approval the connector calls `events.insert` or `events.patch` with `sendUpdates=none`, so Google does not email attendees; the hub never requests `sendUpdates=all`. `CalendarUpdate` refuses fields outside the allowed set (title, start, end, description, location) and refuses any change to attendees or status to cancelled; cancelling and deleting are not in scope.
- Stale events: if the source event's `updated_at` changed after the proposal was created, the approval path requires the existing stale confirmation, as in Gmail drafts.

### D8. Default-engaged kill switch for new connectors
`Connector` registration gains a `default_engaged: bool` on the connector's descriptor. Both new connectors set it to true, so a fresh install has the switch engaged and the status view lists them as "not authorized". Existing connectors keep their current default. The engagement is persisted like any other kill switch; the human releases it with `haste connector release <source>` after the auth verb succeeds.

### D9. Scope tags from herdr workspace labels
Threads and events get project tags through the existing machine-scope mechanism: herdr workspace labels are the source of candidate tags, and the human confirms them. Machine principals see only items whose tags exist on their own machine, enforced in the daemon read path. The scope filter test that already exhausts per-principal reads is extended to `Event` and iMessage threads.

### D10. Audit is counts only
Each iMessage cycle and each calendar cycle writes one entry: source, counts of new threads, messages, events, and any cursor reset, plus the error class. Audit records never include message text, subjects, attendee addresses, event titles, or descriptions. Calendar writes audit `calendar.create` / `calendar.update` with the Google event ID as detail.

### D11. Tests are fakes only
- iMessage: a fake SQLite file built by the test with the observed `message`, `chat`, `handle`, `chat_message_join` schema, including a NULL-text row and a row with the nanosecond date.
- Calendar: a local HTTP server (in the style of the Gmail connector tests) serving `calendarList`, `events.list` with `syncToken`, a 410 response, and `events.insert` / `events.patch`, asserting the `sendUpdates=none` parameter.
- No test reads a real `chat.db` or calls Google. Real access is covered only by the manual smoke group.

## Risks / Trade-offs

- **[chat.db schema drift across macOS versions]** → The spike records the schema on this macOS; the mapper reads only the columns it needs and fails closed with a `Other` error if a required column is missing.
- **[NULL `text` with body in `attributedBody`]** → v1 marks those bodies unavailable; the spike measures how many rows are affected before deciding whether typedstream parsing is worth a follow-up.
- **[Full Disk Access granted to the wrong binary]** → The status view names the exact binary path the daemon runs from; the spike verifies the grant on that binary.
- **[WAL sidecar under read-only open]** → If read-only open cannot see recent rows without `-wal`/`-shm`, the spike chooses between opening read-only with WAL access and a documented latency trade-off; no write to the Messages directory is allowed in any option.
- **[Shared OAuth code refactor breaks Gmail]** → The refactor is behind the existing Gmail tests; those tests must pass unchanged before calendar code lands.
- **[Calendar write mistakes]** → Writes are approval-only, `sendUpdates=none`, no delete, no attendee changes, stale-confirmation on approval.
- **[Collision with `reminders-and-links`]** → Implementation rebases on that change's capability-flag and proposal-kind set before adding calendar kinds.
- **[Google unverified-app warning and scope review]** → Single-user production client, same as Gmail; calendar scopes are sensitive but do not require verification for personal use per the Gmail precedent. The spike confirms the consent screen for the chosen scopes.

## Migration Plan

1. Human spike group completes (FDA, `chat.db` schema, calendar scope consent); findings recorded in `design.md` Open Questions and the specs.
2. Shared OAuth refactor lands with Gmail tests green.
3. Connectors, store tables, and proposal kinds land behind default-engaged kill switches, so the daemon can ship before any authorization.
4. Rollback: engage both kill switches (`haste connector engage imessage|gcal`) or remove the connector registration; the new tables are additive and can stay unused. No Gmail or Messages data is modified in any step.

## Open Questions

- ~~Exact Calendar scope set~~ Answered by the spike: `calendar.events` alone cannot call `calendarList.list` (403 insufficient scopes); with `calendar.calendarlist.readonly` added it works. Decision: request both; calendars are discovered through the list and only those the human selects are synced.
- `chat.db` on this macOS: the `message.date` divisor, the share of NULL-text rows, and whether WAL sidecars are needed for a current read.
- Whether Full Disk Access applies to the built `hasted` binary or to the launching terminal in this setup; the answer sets the status text and the manual smoke steps.
- Whether calendar event attendees' email addresses should be stored as `Person` identifiers or only as display names in v1.

## Spike results (2026-11-09, real accounts)

- Google Calendar consent works with the Desktop client in "Testing" status (the personal project has no homepage or privacy policy URL, so it cannot be published; refresh tokens therefore expire after 7 days and the existing revoked-token path covers re-authorization).
- `calendar.events` alone: events of the primary calendar are readable, `calendarList.list` is denied (403). With `calendar.calendarlist.readonly` added: 4 calendars, all owned by the human.
- A windowed listing returns no `nextSyncToken` (see D5); 50 events per page, more pages follow.
- Not yet answered: `chat.db` is empty on this Mac (Messages is not signed in or Messages in iCloud is off), so the iMessage schema, date unit and NULL-text share are still open. Full Disk Access for the terminal works.
