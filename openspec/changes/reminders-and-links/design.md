## Context

Haste is a Rust workspace: `haste-core` holds the model, the `Connector` trait with its `Guarded` wrapper (kill switch, circuit breaker, bounded transient retries, audit), the policy matrix and the proposal lifecycle. `haste-herdr` already runs an external command with a closed stdin, a hard timeout and process-group kill, and `hasted` spawns it from the scheduler. The connector-framework, data-model, proposals and machine-scope specs define the contracts this change extends. `reminder.create` already exists in the policy matrix as a pending-only action; no code path executes it.

Verified on the real machine before writing this design:

- `/usr/bin/osascript` exists; macOS is 26.6.1.
- `osascript -e 'tell application "System Events" ...'` succeeds from the agent shell, so Apple events work in this GUI session.
- `osascript -e 'tell application "Reminders" to return count of lists'` did not return. It failed after the timeout with `AppleEvent timed out (-1712)`. That is consistent with the Reminders app either waiting on a TCC consent prompt or not being reachable from a non-interactive context. It is the same class of failure the Keychain work hit (-25308 outside the desktop session).
- No Reminders list names, titles or notes were read or printed. The list count is still unknown and must be checked by the human with the auth helper.

Consequence: the design must assume the daemon can run where Reminders access is denied, and must never block or loop on it.

## Goals / Non-Goals

**Goals:**
- Read Apple Reminders into haste on a schedule, incrementally, never overwriting local fields.
- Write to Reminders only through human-approved proposals: `ReminderCreate` and `ReminderComplete`.
- Associate reminders with project tags the same way threads are associated, through haste's own store.
- Create a reminder from an email thread and link the two with a typed link.
- Keep every test free of real Reminders access.

**Non-Goals:**
- Delete, title edit, notes edit, due-date edit, or list move of existing reminders.
- Any agent-initiated direct write.
- Remote machines' own reminders (the hub reaches iCloud Reminders directly).
- Calendar, iMessage, Obsidian, WhatsApp or Messenger.
- Syncing reminder notes into haste beyond a short snippet (see the notes decision).

## Decisions

### D1. EventKit through a small Swift helper, not AppleScript

The helper is a Swift executable built from `crates/haste-connector-reminders/helper/` with the system toolchain. It uses `EKEventStore` with the `reminder` entity type.

Reasons:
- EventKit gives `calendarItemIdentifier`, a stable identifier that matches the contract in the proposal. AppleScript's Reminders `id` is a different, URL-like identifier that is not documented as stable across sync.
- EventKit supports fetch by predicate with a date window, so incremental sync is bounded. AppleScript iterates every reminder and is slow on large lists; the `-1712` timeout above shows how fragile it is.
- EventKit's permission API (`requestFullAccessToReminders`) gives a single explicit authorization step that the auth helper can drive.

Alternatives considered:
- **AppleScript/JXA**: no Rust FFI, but a slow per-item Apple event path, unstable identifiers, and no clean permission request. Kept as a fallback only if the Swift toolchain is unavailable on the target machine.
- **Rust objc2 bindings directly in the daemon**: avoids a second process but puts an Objective-C framework and the TCC prompt inside the long-lived daemon. Rejected: it couples the permission state to the daemon process.

### D2. Helper process contract

The helper speaks one JSON request and one JSON response per invocation, run the same way as the herdr command runner:

- The daemon spawns the helper with `Command`, stdin piped, the request JSON written, then stdin closed.
- Its own process group (`process_group(0)`), killed as a group on timeout.
- A hard timeout per call (a constant in the crate). Timeouts are transient errors.
- The helper's stdout is one JSON object; anything else is an error.

Requests are a typed Rust enum mirrored by the Swift side:

- `Authorization` (status only, never prompts)
- `ListLists`
- `FetchChanged { since: Option<String> }`: returns reminders modified after the cursor, with external id, title, notes truncated to a snippet, due date, completed flag, list name, and last-modified time
- `CreateReminder { title, due: Option<String>, list: Option<String> }` (returns the new external id)
- `CompleteReminder { external_id }`

There is no delete verb and no edit verb. The enum has no other variants, so a Rust-side match is exhaustive and a request for anything else cannot be built.

The helper never prints titles or notes to stderr that the daemon logs. The daemon captures stderr to a bounded buffer and discards it on success; on failure it records only the exit code and error kind.

### D3. Permission (TCC) and the GUI session

- Only the human runs `haste connector reminders auth` from a normal Terminal window. It runs the helper's `Authorization` request with a permission-request flag. macOS shows the prompt in that GUI session. The command prints the resulting status.
- The daemon's own calls use the status-only request. If the status is not granted, the daemon does not prompt and does not retry: it records `permission needed`, engages nothing automatically, and surfaces the state in `haste connector status reminders` and in the TUI status line.
- Permission denial is not a challenge: it does not trip the circuit breaker, because retrying cannot succeed without a human. It is refused fast without invoking the helper on later cycles until the status changes. The scheduler re-checks status once per cycle, which is one cheap call and not a retry loop.
- Authorization errors from the helper map to a dedicated `ConnectorError` variant, not to `Transient`, so the bounded-retry budget is not spent on them.
- The helper is signed with the daemon's bundle identifier or run through the same path used for the existing launchd job, so the TCC grant attaches to the process that actually runs. This is a deployment item in the tasks.

### D4. Connector trait extension for task-like items

Extend the existing trait instead of adding a parallel one, following the `create_draft` precedent:

- New capability flags `reminders_create` and `reminders_complete`, both default off. The read flag is the existing `read`.
- New default trait methods `create_reminder` and `complete_reminder` returning `ConnectorError::Unsupported`. The `Guarded` wrappers gate on the capability flag before invoking the connector, audit the outcome with ids and counts only, and apply the kill switch.
- The Reminders connector overrides only what its capabilities declare.

Alternative considered: a separate `TaskConnector` trait. It is cleaner for a future Todoist or Things connector, but this change has one implementer and the precedent is clear. Revisit when a second task source arrives.

`Reminder` is a new source-agnostic record, so `Source` gains `reminders`. Reminder records do not reuse `Thread`, because they have no participants, messages or triage thread semantics.

### D5. Data model

New `Reminder` record:
- `id` (haste identifier), `source = reminders`, `external_id` (EventKit `calendarItemIdentifier`, unique within source)
- `title`, `notes_snippet` (at most a fixed short length, never logged, never sent to the agent except through the read path that already exposes thread subjects)
- `due_at: Option<Timestamp>`, `completed: bool`, `list_name`
- `remote_modified_at`, `missing_since: Option<Timestamp>`
- `scope_tags: Set<ProjectTag>`, human-only (see D7)
- `version` for stale detection, same rule as threads

Sync rule: a re-sync updates only the remote-owned fields (`title`, `notes_snippet`, `due_at`, `completed`, `list_name`, `remote_modified_at`, `version`). It never touches `scope_tags`, triage state, or links. This is the same rule as the Thread/Triage separation in data-model.

Reminder `scope_tags` live in a separate store table keyed by `(source = reminders, external_id)`, not on the reminder row. A re-sync that rewrites the row therefore cannot drop tags, and a reminder that disappears and reappears keeps its tags (see D7).

Links: reminder kinds are added to `ObjectKind`. The existing closed `LinkType` vocabulary is used unchanged. Email-to-reminder uses `about` (reminder is about the thread). No new link type is introduced.

### D6. Incremental, scheduled sync through the Guarded pattern

- The scheduler runs a reminders cycle on its own interval, separate from mail. The cursor is the last `remote_modified_at` seen, stored with the connector state.
- `FetchChanged` returns at most a fixed batch size; a full batch leaves the cursor at the last item and the next cycle continues.
- One audit record per cycle with counts only: `fetched`, `created`, `updated`, `missing`, `outcome`. No ids beyond the connector name, no titles, no notes.
- Transient failure: bounded attempts (`TRANSIENT_ATTEMPTS_PER_CYCLE`), then wait for the next cycle.
- Permission denied: see D3.

Reminder missing from a later fetch: a reminder is marked `missing_since` only after a full, successful, un-truncated sweep does not return it. Partial sweeps never mark missing. Tags and links are kept while missing.

### D7. Project association and re-sync

EventKit has no tag API, so the association is haste's own data:

- Storage: a `reminder_scope_tags` table keyed by `(reminder external id, tag)`, using the same `ProjectTag` type and the same known-tag set that threads use (union of herdr workspace labels from machine labels, per machine-scope).
- Setting tags: human only, through the human CLI or TUI, and only tags in the known set. Agents and machine principals cannot change them; proposals cannot carry a tag change except as the `project` attribution of a create or complete proposal, which the human approves.
- Optional naming convention, off by default: a human-set mapping from a Reminders list name to a project tag. It is applied only as a suggestion in the TUI and never auto-applied, so a renamed list cannot silently move reminders between projects.
- Re-sync of a reminder seen again: the tags row is looked up by external id and left unchanged. If the list changed, `list_name` updates and tags do not.
- Reminder that disappears and then returns with the same external id: `missing_since` is cleared, tags are kept.
- Reminder deleted in Reminders and never seen again: tags rows are kept for an audit trail but the reminder is hidden from every read. Garbage collection is out of scope.

### D8. Scope filter in the read path

Reminder reads use the same scope filter as threads, in the daemon read path, before serialization:

- Human principal: sees all reminders.
- Machine principal: sees a reminder only if at least one of its `scope_tags` is present in that machine's own workspace labels. A reminder with no tags is visible only to the human.
- Agent principals: same filter as their machine's scope.

The per-principal exhaustive read test in machine-scope is extended with a reminders fixture, so adding a reminder read path that skips the filter fails the test.

### D9. Proposals: ReminderCreate and ReminderComplete

Both are new proposal payloads in the existing lifecycle:

- `ReminderCreate { title, due: Option<Timestamp>, list: Option<String>, project: Option<ProjectTag>, thread_id: Option<ThreadId> }`
- `ReminderComplete { reminder_id, project: Option<ProjectTag> }`

Rules:
- Creation of either proposal follows the non-blocking, idempotent creation rules already in the proposals spec. The `reminder.create` policy outcome is `pending` for agents; `complete` uses a new action `reminder.complete`, also `pending` for agents.
- `project` is the agent's attribution from the herdr workspace hint already implemented in the proposals spec ("Project attribution from the agent's workspace"). It is informational for the approver and, on approval of a create, it becomes the new reminder's scope tag. A tag outside the known set is rejected at creation.
- Approval is a human action (existing rule). Approval does not write to Reminders. Send/execute is a separate human action, as today.
- Execution: `approved` to `sent` only when the helper call succeeds. A create records the new external id and the reminder row immediately, in the same transaction as the `sent` transition.
- A complete on a reminder whose external id is missing or already completed is refused with a validation error and the proposal stays `approved`; the human decides.
- Crash between helper success and the `sent` transition: the proposal is recorded as `executing` before the helper call. On daemon start, an `executing` proposal is not re-run. It is shown to the human as needing check, because re-running a create could duplicate a reminder. There is no automatic retry.
- Stale check: a complete proposal references the reminder `version`; a changed reminder (title, due, completed) sets `stale` and requires acknowledgement, as threads do.

Reminder notes on create: the created reminder's notes carry only the haste thread or proposal identifier as a back-reference (for example `haste:thr_...`). No subject, no body, no participant. This is decided here: it is safe because identifiers reveal nothing outside haste, and it lets a human who sees the reminder in Reminders find the thread. It is written once at creation and never edited, so there is no notes-edit path.

### D10. Email to reminder

- Human action (TUI key or `haste email to-reminder <thread>`): creates a `ReminderCreate` proposal, which the human then approves. There is one write path, through the proposal record, not a second path that writes to Reminders directly. The CLI shortcut may create and approve in one invocation for the human, but it still goes through the proposal record and the audit log.
- Agent action: `haste propose reminder --from-thread <id>` creates a `ReminderCreate` proposal with title taken from the thread subject (truncated) and `thread_id` set.
- On execution, the daemon writes a `about` link from the reminder to the thread, in the links table. Either side can then find the other: thread view lists reminders linked to it, reminder view lists its linked threads.
- A link to a thread that no longer exists is rejected by the existing link validation.

### D11. Human-only admin

`haste connector reminders auth`, `haste connector reminders status`, the kill-switch verbs (`haste connector status|release|engage reminders`, already generic), and tag changes are human-only and live in the human CLI build, as the existing admin verbs do. Agent tokens cannot reach them.

## Risks / Trade-offs

- [Reminders access stays denied on the daemon's machine] → Status shows `permission needed`, no retries, proposals for reminders are refused with a clear reason. The auth helper is the only way to fix it.
- [Swift toolchain missing on the target machine] → Build step documented in README; the connector is disabled, not broken, when the helper binary is absent.
- [Crash after a successful create] → `executing` state and no automatic re-run (D9). Cost: a human sometimes checks a reminder by hand.
- [Tag association lost when a reminder is deleted and recreated in Reminders] → Accepted: a new external id is a new reminder. The known-tag naming convention is the escape hatch for users who want list-based defaults.
- [Notes back-reference reveals haste identifiers in Reminders] → Identifiers only; no message content. Accepted.
- [Large initial sync] → Batch size and cursor; the first cycle may take several cycles.
- [Helper output schema drift between Swift and Rust] → Typed enums on both sides and a fixture test that round-trips the JSON.
- [Scope leak if a reminder read path skips the filter] → Exhaustive per-principal test extended to reminders (D8).

## Migration Plan

1. Add the schema migration for reminders, reminder scope tags and reminder links; no existing table changes shape.
2. Ship the helper and connector with the kill switch engaged by default. Nothing syncs until the human releases it.
3. Human runs `haste connector reminders auth` from Terminal, then releases the switch. Status shows the list count and last cycle counts only.
4. Rollback: engage the kill switch; disable the scheduler job. Reminder rows and tags remain as inert data. No Reminders data is ever deleted by haste.

## Open Questions

- **Exact status of Reminders access from the daemon's launch path** (launchd versus Terminal) is not verified. The `-1712` result suggests the daemon must be launched in the user's GUI session or the helper must be granted directly. The auth helper must confirm which process gets the TCC grant before the release step. This is a blocking check for task group 1.
- **Helper bundle identity** for a stable TCC grant: whether an ad-hoc-signed helper keeps its grant across rebuilds needs a manual check on the real machine.
- **Notes snippet length**: the constant is a placeholder until the human chooses it.
- **Whether `missing_since` should be surfaced in the TUI** or only in the status output. Default: status output only.
