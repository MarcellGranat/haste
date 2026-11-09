## 1. Real-machine verification and decisions (blocking)

- [ ] 1.1 Human runs a helper spike from a normal Terminal window to confirm EventKit authorization prompts and reports granted or denied, and records which process received the grant (Terminal versus launchd daemon)
- [ ] 1.2 Confirm that the daemon's launch path can read Reminders status without prompting, or record that it cannot and adjust the auth flow in design D3
- [ ] 1.3 Confirm the list count and the reminder count per list from EventKit with a count-only query (no titles, notes or list names printed) and record the numbers in the design open questions
- [ ] 1.4 Check whether an ad-hoc-signed helper keeps its TCC grant across rebuilds, and record the signing decision in design D3

## 2. Data model and store

- [x] 2.1 Add `Reminders` to `Source` and a `Reminder` record type in `haste-core` with serde round-trip tests
- [x] 2.2 Add reminder kinds to `ObjectKind`, keeping `LinkType` unchanged, with a test that the link vocabulary is still closed
- [x] 2.3 Add a store migration for the `reminders` table (external id unique per source, bounded snippet, `missing_since`, `version`)
- [x] 2.4 Add the `reminder_scope_tags` table keyed by `(external_id, tag)` and store methods that validate tags against the known set
- [x] 2.5 Add a store method that re-syncs remote-owned fields only and a test that tags, triage and links survive a re-sync
- [x] 2.6 Add the `reminder_links` path through the existing links table with validation that both endpoints exist

## 3. Helper protocol and runner

- [x] 3.1 Define the typed `RemindersRequest` enum (`Authorization`, `ListLists`, `FetchChanged`, `CreateReminder`, `CompleteReminder`) and the response types in `haste-connector-reminders`, with no delete or edit variant
- [x] 3.2 Implement the runner with closed stdin, a hard timeout, process-group kill and single-object stdout parsing, modelled on the herdr command runner
- [x] 3.3 Add a bounded stderr capture that is discarded on success and reduced to exit code and error kind on failure
- [x] 3.4 Add unit tests for timeout kill, malformed output and non-zero exit using a fake helper executable script in the test fixtures

## 4. Swift helper

- [x] 4.1 Create the Swift helper package under `crates/haste-connector-reminders/helper/` with an `EKEventStore` reminder entity and the five request handlers
- [x] 4.2 Implement `Authorization` as status-only and `Authorization` with a prompt flag used only by the human auth command
- [x] 4.3 Implement `FetchChanged` with truncated notes, due date, completed flag, list name and modified time, returning at most the batch size
- [x] 4.4 Implement `CreateReminder` and `CompleteReminder` with a check that the target exists and is not already completed
- [x] 4.5 Add a build script that compiles the helper with the system toolchain and places it where the daemon looks for it, and a clear error when the binary is absent
- [x] 4.6 Add a JSON round-trip fixture test shared with the Rust side so schema drift fails the build

## 5. Connector and guarded calls

- [x] 5.1 Implement the Reminders connector with capability flags `read`, `reminders_create`, `reminders_complete` and no draft, send or archive
- [x] 5.2 Extend the `Connector` trait with default-unsupported `create_reminder` and `complete_reminder` and the matching `Guarded` wrappers, gated by capability flags and audited
- [x] 5.3 Add `Guarded` tests that a connector without the flag is never invoked and that default methods return unsupported
- [x] 5.4 Implement the permission state machine: status-only in the daemon, `permission needed` with no retry and no circuit-breaker trip, and a dedicated authorization error variant
- [x] 5.5 Implement incremental fetch with cursor, batch size, missing-only-after-full-sweep and partial-sweep safety
- [x] 5.6 Implement the per-cycle audit record with counts only and a test that titles and notes never appear in audit output
- [x] 5.7 Engage the kill switch on first install and test that no call runs until release

## 6. Policy and proposals

- [x] 6.1 Add the `reminder.complete` action to the policy matrix and set `reminder.create` and `reminder.complete` to `pending` for agents, never `auto`
- [x] 6.2 Add the `ReminderCreate` and `ReminderComplete` payloads to the proposal store
- [x] 6.3 Add the `executing` proposal state with the transitions in the modified lifecycle requirement and tests for each permitted and refused transition
- [x] 6.4 Implement execution: approved to executing, helper call, then sent with the reminder row and tag in one transaction, or back to approved on failure
- [x] 6.5 Implement the startup rule that leaves `executing` proposals untouched and lists them as needing a check
- [x] 6.6 Implement create-time project validation against the known tag set and the stale check for `ReminderComplete` using reminder version

## 7. Scope and read path

- [x] 7.1 Add reminder reads to the daemon read path behind the single scope filter
- [x] 7.2 Extend the exhaustive per-principal read test with a reminders fixture so a read that skips the filter fails
- [x] 7.3 Test that a machine principal sees a tagged reminder only when its workspace labels contain the tag, and that untagged reminders are human-only

## 8. Email to reminder and links

- [x] 8.1 Add the `haste email to-reminder <thread>` human CLI action that creates a `ReminderCreate` proposal referencing the thread
- [x] 8.2 Add the agent proposal path `haste propose reminder --from-thread <id>` with idempotency key
- [x] 8.3 Store the `about` link from reminder to thread in the execution transaction and test that no link is stored on failure
- [x] 8.4 Implement the back-reference notes with identifiers only and test that subject, sender and body text are absent
- [x] 8.5 Add the TUI key (`T`) on a thread to start email-to-reminder
- [ ] 8.6 Show linked reminders on the thread view (deferred: the TUI has no thread detail view yet)

## 9. Human CLI and TUI

- [x] 9.1 Add `haste connector reminders auth` (human-only, prompts once, prints the status) and `haste connector reminders status` (state, kill switch, last cycle counts, no titles)
- [x] 9.2 Add reminder tag set and remove commands for the human CLI, human-only
- [ ] 9.5 Add reminder tagging to the TUI (deferred: the TUI has no reminders list view yet)
- [x] 9.3 Render `ReminderCreate` and `ReminderComplete` proposals in the approvals view with title and project, and the `executing` needs-check state
- [x] 9.4 Show `permission needed` and the reminder counts in the status view

## 10. Tests and quality

- [x] 10.1 Add an end-to-end test in `hasted` using the fake helper executable covering sync, create-from-thread approval, complete approval, and restart with an `executing` proposal
- [x] 10.2 Run `cargo test` and `cargo clippy --all-targets -- -D warnings` on the workspace and fix findings
- [x] 10.3 Run `cargo fmt --check`

## 11. Documentation

- [x] 11.1 Add a README section for the Reminders connector: build the helper, run the auth command from Terminal, release the kill switch, tag reminders
- [x] 11.2 Document the email-to-reminder action and the proposal flow in the README
- [x] 11.3 Document the known limits: macOS only, no delete or edit, back-reference content, and the crash-check rule

## 12. Manual smoke check against the real Reminders app

- [ ] 12.1 Human runs `haste connector reminders auth` from Terminal and grants access
- [ ] 12.2 Human releases the Reminders kill switch and confirms the status shows counts only
- [ ] 12.3 Human tags one existing reminder with a known project and confirms that a machine principal sees it only on that machine
- [ ] 12.4 Human approves one `ReminderCreate` from a test thread and confirms the reminder appears in Reminders with only the identifier in its notes
- [ ] 12.5 Human approves one `ReminderComplete` for the test reminder and confirms it is completed in Reminders, then removes it manually in Reminders (haste never deletes)
