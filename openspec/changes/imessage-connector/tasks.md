## 1. Human spike (upfront, blocked until Messages is signed in or Messages in iCloud is enabled on the hub Mac)

- [ ] 1.1 Sign in to Messages / enable Messages in iCloud on the hub Mac and confirm `chat.db` is no longer empty (currently 0 messages, 0 chats)
- [ ] 1.2 Confirm Full Disk Access: it already works for the terminal; determine whether it applies to the built daemon binary or to the launching terminal, confirm a read-only open of `~/Library/Messages/chat.db` succeeds and fails without it, and record the binary path and the observed error text
- [ ] 1.3 Record the `chat.db` schema on this macOS version (`message`, `chat`, `handle`, `chat_message_join` columns), the `message.date` unit, the share of rows with NULL `text`, and whether recent rows are visible without the `-wal`/`-shm` sidecars under read-only open; update design.md Open Questions with the answers
- [ ] 1.4 Human sign-off on the spike findings before section 2 starts

## 2. Core model and policy

- [ ] 2.1 Confirm `Source::Imessage` (name `imessage`) round-trips in `haste-core`
- [ ] 2.2 Add the iMessage `Draft` proposal payload (plain text body, target an iMessage thread)
- [ ] 2.3 Confirm `imessage.send` resolves to `denied` in the policy gate as an undefined action
- [ ] 2.4 Confirm the default-engaged flag on the connector descriptor (provided by the connector framework) is available

## 3. iMessage connector

- [ ] 3.1 Create `crates/haste-connector-imessage` with the `Connector` implementation, capabilities `read` and `draft` only, and no send method
- [ ] 3.2 Implement the read-only `chat.db` open, mapping `PermissionNeeded` on permission errors with no retry
- [ ] 3.3 Implement ROWID-cursor incremental sync with bounded first backfill and idempotent message upsert
- [ ] 3.4 Implement the chat to thread, message to message, handle to person, and Messages-epoch date mapping, with NULL-text rows marked body-unavailable
- [ ] 3.5 Implement the per-cycle counts-only audit entry
- [ ] 3.6 Register the connector with default-engaged kill switch and the `haste connector release|engage|status imessage` verbs in the human build
- [ ] 3.7 Implement the draft approve path that marks "approved for manual copy" with no transmission, and the TUI draft view
- [ ] 3.8 Build the fake `chat.db` test fixture with the observed schema, a NULL-text row, and a nanosecond-date row; add tests for read-only handle, ROWID delta, idempotency, PermissionNeeded, date conversion, and audit content
- [ ] 3.9 Add a test that the connector directory's mtimes and contents are unchanged after a sync

## 4. Daemon, scope and TUI

- [ ] 4.1 Wire the connector into the scheduler through `Guarded` with its own interval and kill switch
- [ ] 4.2 Extend the exhaustive per-principal scope-filter test to iMessage threads
- [ ] 4.3 Render iMessage drafts in the approvals view, and show the `PermissionNeeded` and default-engaged states in the connectors status
- [ ] 4.4 Add the `haste connector status` line for `imessage` in the human build

## 5. Docs and checks

- [ ] 5.1 Add a README section for the Full Disk Access step
- [ ] 5.2 Run `cargo test` for the workspace and the new crate's tests, with no test reading a real Messages database
- [ ] 5.3 Run `cargo clippy` with warnings as errors on the changed crates and `cargo fmt --check`

## 6. Manual smoke (end, human-run on this Mac, real accounts)

- [ ] 6.1 Fresh install: start the daemon and confirm `haste connector status` lists `imessage` as engaged, with no reads performed
- [ ] 6.2 Without Full Disk Access, release `imessage` and confirm the status shows PermissionNeeded naming the binary, with no retry loop in the audit log
- [ ] 6.3 Grant Full Disk Access, release `imessage`, and confirm one cycle adds threads and messages with counts-only audit and no message text in the audit log
- [ ] 6.4 Confirm a sent iMessage (sent from the Messages app) appears on the next cycle, and that haste never transmits anything
- [ ] 6.5 Propose an iMessage draft, approve it, and confirm it is marked approved for manual copy and nothing was sent
- [ ] 6.6 Set herdr workspace labels as project tags on iMessage threads
- [ ] 6.7 Engage `imessage` again and confirm all reads are refused
