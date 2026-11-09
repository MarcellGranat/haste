## Why

Haste already reads mail and proposes actions, but tasks the user tracks in Apple Reminders live outside the hub. An agent working in a herdr workspace cannot mark a task done, and an email that needs follow-up cannot be turned into a reminder without copy-and-paste. Reminders live in the user's iCloud account, so the hub can reach them directly on the machine that runs the daemon. This change adds a read-only Reminders connector, a write path that only runs through human-approved proposals, and typed links between mail threads and reminders.

## What Changes

- Read Apple Reminders into haste as a new `Reminder` record (id, title, short notes snippet that is never logged, due date, completed flag, list name, external id equal to the EventKit `calendarItemIdentifier`, source `reminders`). Sync is incremental, scheduled, and never overwrites local fields such as scope tags or triage state.
- Reminders reads go through the existing `Connector` / `Guarded` pattern: per-connector kill switch, circuit breaker, bounded retries, per-cycle audit with counts only.
- Reminder writes happen only through approved proposals. An approved `ReminderCreate` proposal creates a reminder (title, optional due date, optional project). A new `ReminderComplete` proposal marks a reminder done; an agent in a herdr workspace proposes it and the proposal carries the workspace name as its project.
- Nothing is created, completed, edited or deleted in Reminders without a human approving. Agents never write directly. There is no delete and no title edit in this change.
- Email to reminder: a human TUI/CLI action and an agent proposal create a reminder from a thread (title from the subject, link back to the thread) and record a typed link (`about` or `references` from the existing closed vocabulary) so either side can find the other.
- Project tags on reminders: reminders carry `scope_tags` like threads, set only by the human. The existing scope filter (visible to the human, machine principals see only tags present on their own machine) covers reminder reads in the daemon read path, and the exhaustive per-principal read test is extended to reminders.
- Permission handling: a human-run `haste connector reminders auth` command triggers the macOS Reminders permission prompt from a normal Terminal session. Status reports `permission needed` and the daemon never retries in a loop when access is denied.
- Technical approach (EventKit via a small Swift helper, or AppleScript/JXA) is decided in `design.md`.

Out of scope for this change: iMessage, Calendar, Obsidian, WhatsApp/Messenger, and the reminders of remote machines (the hub reaches the user's iCloud Reminders directly).

**BREAKING**: none for existing behaviour. The `reminder.create` action changes from a pending-only stub to a real write path behind approval.

## Capabilities

### New Capabilities
- `connector-reminders`: Apple Reminders read sync, the Swift helper process contract, the TCC permission flow, and approved create/complete writes.
- `reminder-links`: Creating a reminder from an email thread, the typed link between thread and reminder, and the back-reference policy.

### Modified Capabilities
- `data-model`: adds the `Reminder` record, its source value, and reminder-specific fields; reminder re-sync preserves local fields; typed links accept reminder kinds.
- `proposals`: adds the `ReminderCreate` and `ReminderComplete` payloads, their project attribution, and execution on approval.
- `machine-scope`: reminder `scope_tags` are set by the human only and filter reminder reads in the daemon read path.
- `approvals-tui`: approval views render reminder proposals with title and project, and show the permission-needed state from the reminders connector.
- `connector-framework`: the capability flag set gains `reminders_create` and `reminders_complete`, and the task-like trait methods are default-unsupported and gated.
- `policy-gate`: adds the `reminder.complete` action, pending for agents and never automatic.

## Impact

- Code: `crates/haste-core` (model, connector, policy, proposal), `crates/haste-connector-reminders` (new crate with the connector and the helper protocol), a Swift helper source under the new crate, `crates/hasted` (scheduler, dispatch, read-path scope filter), `crates/haste` (human CLI: `connector reminders auth|status`, `email to-reminder`), `crates/haste-tui` (approval rendering and the email-to-reminder action), `crates/haste-store` (new tables for reminders, reminder scope tags, and reminder links), `crates/haste-proto`.
- Systems: macOS only for the connector. EventKit requires the Reminders TCC permission granted to the process that runs the helper.
- Dependencies: no new Rust crates expected beyond serde already in use. The Swift helper is compiled with the system toolchain.
- Tests: no test needs real Reminders; a fake helper executable (like the fake herdr script) drives the connector tests.
- Docs: README section for the Reminders connector and the auth step.
- Audit: records carry reminder ids and counts only, never titles or notes.
