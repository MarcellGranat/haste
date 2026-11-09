## Context

The hub already has the pattern this change needs. Each connector implements the `Connector` trait, the daemon calls it only through `Guarded` (kill switch, capability flags, circuit breaker, bounded transient retry, audit), and the Gmail connector shows the OAuth + Keychain + fake-server test shape. There is no calendar source or event record yet. The `reminders-and-links` change is in flight and touches the same capability flags and proposal kinds, so this design keeps its additions narrow.

Constraints from the request: calendar means Google Calendar only; agents may propose calendar creates and updates, a human approves each one; no invitations are sent by the hub; no deletes; kill switch engaged on first install; per-cycle audit with counts only; project tags from herdr workspace labels; scope filter for machine principals; fakes-only tests.

## Goals / Non-Goals

**Goals:**
- Read Google Calendar events into the unified model, incrementally and read-only.
- Let agents propose calendar creates and updates and let the human approve or reject them.
- Keep the blast radius of the calendar consent separate from Gmail.
- Make the connector safe on first install: kill switch engaged until the human authorizes and releases.

**Non-Goals:**
- Apple Calendar / EventKit. Calendar means Google Calendar.
- Deleting events, changing attendees' invitations, or sending invitation emails.
- Changing Gmail scopes or the `com.haste.gmail` Keychain item.
- iMessage (see the parked change `imessage-connector`).

## Decisions

### D1. Google Calendar reuses the Gmail OAuth client and PKCE flow, with a separate consent and Keychain item
The consent and token code in `haste-connector-gmail` is moved behind a small shared module (or the gcal crate depends on it) parameterised by `(scopes, keychain_service)`. The calendar connector uses the same Google Cloud OAuth client ID, PKCE, loopback redirect on `127.0.0.1`, and the same refresh-then-access-token flow, but stores its refresh token under `com.haste.gcal`.
- Alternative: incremental authorization that adds a calendar scope to the Gmail refresh token. Rejected: it widens the Gmail token, couples revocation of calendar and Gmail, and makes a Gmail-only checkpoint kill calendar too.
- Scopes (confirmed by the spike): `calendar.events` (read and approved write of events) plus `calendar.calendarlist.readonly` (listing the human's calendars). `calendar.readonly` is not requested because it cannot write.
- The app stays in Testing status; the consent screen shows the unverified-app warning, which the human acknowledges, as in Gmail.

### D2. Calendar read is incremental with `syncToken`
Google only returns a `nextSyncToken` for a listing made without `timeMin`/`timeMax` (documented restriction on incremental sync; the spike saw no token on a windowed first page). So the first cycle lists each selected calendar **without a time window**, pages to the end to get the `nextSyncToken`, and **stores only events inside the window** (now minus `CAL_BACKFILL_DAYS`, now plus `CAL_LOOKAHEAD_DAYS`, both compile-time constants; recurring series are kept as one master event, not expanded). Events outside the window are discarded locally. Later cycles pass the token and receive changed and cancelled events. A `410 Gone` on the token resets that calendar to a fresh full listing once and does not loop.

### D3. Calendar selection is config, not a blocker
Only calendars the human lists in `gcal.json` (non-secret, mode `0600`, in the data directory) are read. The file holds calendar IDs only. Implementation does not wait for the human to choose calendars or herdr workspace tags: the connector reads whatever the file lists (an empty or missing list means nothing is read). The human fills the file in, and sets tags, in the final manual smoke group.

### D4. Calendar events map to a new `Event` record
`Event { source: gcal, external_id: "<calendarId>/<eventId>", calendar, title, start, end, all_day, location_present, status, attendees, updated_at }`. Attendees map to `Person` via the existing identifiers. Description and location text are stored only in the local store, never in audit or logs. Event sync never overwrites local fields (scope tags, triage state), matching the reminders rule.

### D5. Calendar writes are proposals executed only on approval
Two new proposal payloads: `CalendarCreate { calendar, title, start, end, attendees?, description? }` and `CalendarUpdate { event_ref, changed_fields }`. Both carry the herdr workspace name as project when an agent proposes them. Policy: `calendar.create` and `calendar.update` are `pending` for agents and never automatic. On approval the connector calls `events.insert` or `events.patch` with `sendUpdates=none`, so Google does not email attendees; the hub never requests `sendUpdates=all`. `CalendarUpdate` refuses fields outside the allowed set (title, start, end, description, location) and refuses any change to attendees or status to cancelled; cancelling and deleting are not in scope.
- Stale events: if the source event's `updated_at` changed after the proposal was created, the approval path requires the existing stale confirmation, as in Gmail drafts.

### D6. Default-engaged kill switch for new connectors
`Connector` registration gains a `default_engaged: bool` on the connector's descriptor. The calendar connector sets it to true, so a fresh install has the switch engaged and the status view lists it as "not authorized". Existing connectors keep their current default. The engagement is persisted like any other kill switch; the human releases it with `haste connector release gcal` after the auth verb succeeds.

### D7. Scope tags from herdr workspace labels
Events get project tags through the existing machine-scope mechanism: herdr workspace labels are the source of candidate tags, and the human confirms them. Machine principals see only items whose tags exist on their own machine, enforced in the daemon read path. The scope filter test that already exhausts per-principal reads is extended to `Event`.

### D8. Audit is counts only
Each calendar cycle writes one entry: source, counts of new and changed events, and any cursor reset, plus the error class. Audit records never include subjects, attendee addresses, event titles, or descriptions. Calendar writes audit `calendar.create` / `calendar.update` with the Google event ID as detail.

### D9. Tests are fakes only
A local HTTP server (in the style of the Gmail connector tests) serving `calendarList`, `events.list` with `syncToken`, a 410 response, and `events.insert` / `events.patch`, asserting the `sendUpdates=none` parameter. No test calls Google. Real access is covered only by the manual smoke group.

## Risks / Trade-offs

- **[Shared OAuth code refactor breaks Gmail]** -> The refactor is behind the existing Gmail tests; those tests must pass unchanged before calendar code lands.
- **[Calendar write mistakes]** -> Writes are approval-only, `sendUpdates=none`, no delete, no attendee changes, stale-confirmation on approval.
- **[Collision with `reminders-and-links`]** -> Implementation rebases on that change's capability-flag and proposal-kind set before adding calendar kinds.
- **[Google unverified-app warning and 7-day refresh tokens]** -> Single-user client in Testing status, same as Gmail; refresh tokens expire after 7 days and the existing revoked-token path (`Checkpoint`, re-authorization notice) covers it.

## Migration Plan

1. Human sign-off on the spike findings.
2. Shared OAuth refactor lands with Gmail tests green.
3. Connector, store tables, and proposal kinds land behind a default-engaged kill switch, so the daemon can ship before any authorization.
4. Rollback: engage the kill switch (`haste connector engage gcal`) or remove the connector registration; the new tables are additive and can stay unused. No Gmail data is modified in any step.

## Open Questions

- Whether calendar event attendees' email addresses should be stored as `Person` identifiers or only as display names in v1.

## Spike results (2026-11-09, real accounts)

- Google Calendar consent works with the Desktop client in "Testing" status (the personal project has no homepage or privacy policy URL, so it cannot be published; refresh tokens therefore expire after 7 days and the existing revoked-token path covers re-authorization).
- `calendar.events` alone: events of the primary calendar are readable, `calendarList.list` is denied (403 insufficient scopes). With `calendar.calendarlist.readonly` added: 4 calendars, all owned by the human. Decision: request both scopes; calendars are discovered through the list and only those the human selects in `gcal.json` are synced.
- A windowed listing returns no `nextSyncToken` (see D2); 50 events per page, more pages follow.
