## 1. Workspace and shared crates

- [x] 1.1 Create Cargo workspace with members `haste-proto`, `haste-core`, `haste-store`, `hasted`, `haste`, `haste-tui` and pin shared dependencies (tokio, rusqlite, clap, serde, serde_json, ratatui, crossterm, sha2)
- [x] 1.2 Define `haste-proto` request/response types, protocol version constant, and length-prefixed JSON framing with a maximum frame size constant
- [x] 1.3 Add framing unit tests covering round trip, unknown version, and oversized frame rejection

## 2. Data model and store

- [x] 2.1 Define `haste-core` types for Thread, Message, Person, Source (closed enum with `Other`), triage state, and link type vocabulary
- [x] 2.2 Write initial SQLite migration (threads, messages, people, identifiers, triage, links, proposals, principals, tokens, connector_state) with `user_version` tracking
- [x] 2.3 Implement single-writer task with bounded channel and read-only connection pool in `haste-store`
- [x] 2.4 Implement repositories for thread listing with source filter and archived/snoozed exclusion
- [x] 2.5 Implement link insertion with vocabulary validation and existence checks
- [x] 2.6 Add tests: triage survives re-sync upsert, duplicate `(source, external_id)` updates in place, identifier uniqueness, unknown link type rejected, nonexistent link target rejected

## 3. Policy gate

- [x] 3.1 Implement static capability matrix keyed by `(principal, action)` with `auto | pending | denied` outcomes matching the decided matrix
- [x] 3.2 Implement `note.append` path predicate: canonicalize, enforce `people/**` and `tasks/**` prefixes, reject symlink components
- [x] 3.3 Implement pure `evaluate(principal, action, target, matrix) -> Decision` function
- [x] 3.4 Add exhaustive tests asserting every `(action, outcome)` pair in the decided matrix, traversal attempts, symlink component rejection, and that `archive`/`snooze` never resolve to `auto`
- [x] 3.5 Add test that `send` resolves to denied for every agent and machine principal

## 4. Audit log

- [x] 4.1 Implement append-only JSONL writer with `O_APPEND`, file mode `0600`, sequence numbers, and SHA-256 previous-record hash chain
- [x] 4.2 Implement chain verification function returning the first broken sequence number
- [x] 4.3 Add tests: chain verification detects modified and reordered records, file mode is `0600`, records omit note and message bodies

## 5. Agent authentication

- [x] 5.1 Implement token generation (256-bit, CSPRNG), salted hash storage, and one-time display at registration
- [x] 5.2 Implement token-to-principal resolution and allowed-action subset check
- [x] 5.3 Implement revocation and ensure revoked tokens are rejected
- [x] 5.4 Ensure herdr environment variable is used only as a token lookup hint and is never accepted as identity
- [x] 5.5 Add tests: forged environment identifier ignored, missing token rejected, action outside subset rejected, token material absent from error and audit output

## 6. Proposals

- [x] 6.1 Implement proposal creation with ULID identifiers, required idempotency key, and `(principal, idempotency_key)` uniqueness
- [x] 6.2 Implement lifecycle state machine with the permitted transitions only; illegal transitions return an error and leave state unchanged
- [x] 6.3 Implement TTL expiry scheduler task (default 7 days, configurable)
- [x] 6.4 Implement supersession on new proposal with the same principal, action, and source thread
- [x] 6.5 Implement stale-thread detection comparing recorded and current thread version before approve and send, requiring acknowledgement when stale
- [x] 6.6 Implement approve (human-only) and send (human-only, requires `approved`) as separate operations
- [x] 6.7 Implement result delivery through herdr `agent.prompt` with three attempts and backoff, recording each failure in the audit log
- [x] 6.8 Implement `haste proposal status` backed by daemon request, scoped to the calling principal
- [x] 6.9 Add tests: idempotent retry returns same proposal, illegal transitions refused, agent cannot approve or send, send refused unless approved, stale flag set on version bump, supersession, delivery retry bound

## 7. Daemon

- [x] 7.1 Implement `hasted` socket server: create `0700` runtime directory, refuse to start if directory permissions are looser, bind Unix socket, no TCP listener
- [x] 7.2 Implement request dispatch that authenticates the token, consults the policy gate, executes allowed actions, and creates proposals for `pending` outcomes
- [x] 7.3 Implement `note.append` executor: open with `O_APPEND|O_NOFOLLOW`, write attribution comment line then text, never modify existing bytes
- [x] 7.4 Implement subscription push for thread, proposal, and approval change events
- [x] 7.5 Implement store migration on startup and refusal when stored schema version is newer than supported
- [x] 7.6 Create launchd plist template and document `launchctl bootstrap` and `bootout` steps in the repository README
- [x] 7.7 Add integration test: start daemon on temporary socket directory, exercise a read, a draft proposal, and a note append end to end

## 8. Connector framework

- [x] 8.1 Define `Connector` trait with `source()`, `capabilities()`, `fetch()`, and `send()`, and choose the async-trait approach consistently
- [x] 8.2 Implement capability gating in the daemon so methods are not invoked without the matching flag
- [x] 8.3 Implement persisted per-connector kill switch checked before every call
- [x] 8.4 Implement circuit breaker: challenge and checkpoint errors engage the kill switch immediately with no retry and emit a TUI notification
- [x] 8.5 Implement bounded transient retry (fixed attempt budget per cycle, then wait for next cycle)
- [x] 8.6 Add tests using an in-tree test connector: capability refusal, kill switch persists across restart, checkpoint trips breaker with zero retries, transient budget respected

## 9. CLI

- [x] 9.1 Implement `haste` with clap subcommands for read operations (thread list with `--source` filter, thread show), `draft`, `archive`, `snooze`, `reminder create`, `note append`, and `proposal status`
- [x] 9.2 Confirm no send subcommand exists in the agent-facing CLI build
- [x] 9.3 Implement human-only subcommands for agent registration, token revocation, and proposal approve/reject, gated behind the human shell configuration
- [x] 9.4 Add tests for CLI parsing and that the `send` verb is absent

## 10. TUI shell and approvals view

- [x] 10.1 Implement ratatui application shell with crossterm event loop, daemon client connection, and subscription handling
- [x] 10.2 Implement unified thread list with per-source filter
- [x] 10.3 Implement approvals view: list of pending and approved proposals with vi keys (`j`, `k`, `g`, `G`, `/`, `Enter`)
- [x] 10.4 Implement per-agent grouping with collapse and expand
- [x] 10.5 Implement batch approval with mark, count-bearing confirmation, and per-item acknowledgement for stale proposals
- [x] 10.6 Implement reject action for pending proposals
- [x] 10.7 Implement human-only send action for approved proposals with recipient and source confirmation
- [x] 10.8 Add tests for key handling and grouping state logic that do not require a live terminal

## 11. Quality gates and documentation

- [x] 11.1 Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` and resolve all findings
- [x] 11.2 Document the threat model, token issuance flow, and roadmap stubs for later connector changes in the repository README
- [x] 11.3 Verify each spec scenario is covered by at least one test or by a documented manual check
