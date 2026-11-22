## 1. Pure logic (haste-core, no I/O)

- [ ] 1.1 Add `haste-core` module for memory YAML parsing with `serde_yaml_ng` (add the dependency; unknown keys ignored, size limits) with unit tests for valid, malformed, oversize and invalid-pattern files
- [ ] 1.2 Add memory trigger matching by channel and by sender, reusing the `IgnoreList` regex engine, with tests for OR semantics, disabled memories and the 3-memory / 1 KB caps
- [ ] 1.3 Add memory fingerprint (SHA-256 over matched ids and bodies) with a test that an edit changes it
- [ ] 1.4 Add the resource gate as a pure function over sampled load, core count, free MB and pressure, with the constants block; tests for busy, free, and failed-sample (fail closed)
- [ ] 1.5 Add the prompt budget trimming function (drop oldest messages, then memory bodies) with a test per rule
- [ ] 1.6 Add the TODO suggestion type and its validation (length limits, date parsing with invalid date to null, priority enum) with tests

## 2. Store

- [ ] 2.1 Add forward-only migration creating table `ai_results` (kind, thread_id, thread_version, memory_fingerprint, payload, outcome, created_at) and a unique key on the four key columns
- [ ] 2.2 Add store methods: get by key, put, delete rows older than 14 days, with tests against an in-memory store
- [ ] 2.3 Implement an in-memory job queue with dedupe per (kind, thread, version) and `next_attempt_at`; jobs are not persisted, so after a daemon restart they are re-enqueued on the next thread open

## 3. Protocol

- [ ] 3.1 Add `AiSuggestionGet`, `AiRewrite` and `AiMemoriesList` request and response types to `haste-proto`
- [ ] 3.2 Bump `PROTOCOL_VERSION` from 11 to 12 and update the version test
- [ ] 3.3 Map the three requests to a human-only action in `haste-core` policy and in `dispatch.rs`'s `action_for` and `is_human_only`, with a test that agent and machine principals are denied

## 4. Daemon apfel module

- [ ] 4.1 Create `crates/hasted/src/apfel.rs` with the CONSTANTS block (`APFEL_QUEUE_DEPTH`, `APFEL_CALL_TIMEOUT`, `APFEL_MAX_LOAD_PER_CORE`, `APFEL_MIN_FREE_MB`, `APFEL_PROMPT_TOKEN_BUDGET`, cache retention) and the settings file `<data-dir>/apfel.json` (defaults to disabled)
- [ ] 4.2 Implement the runner: spawn `apfel` with `--messages -` or `--system-file`, stdin delivery, `--count-tokens` preflight, timeout with kill, no `--retry`, no content in argv
- [ ] 4.3 Implement the temporary prompt file in the run directory (0700 dir, 0600 file) and its removal on every outcome
- [ ] 4.4 Implement the TODO schema file written at daemon start and the `--schema` call with validation to `invalid_output`
- [ ] 4.5 Implement resource sampling (`sysctl -n vm.loadavg`, `sysctl -n hw.ncpu`, `memory_pressure`) behind a small trait so tests can inject values
- [ ] 4.6 Implement `apfel --model-info` availability check at start and the `unavailable` outcome
- [ ] 4.7 Add the scheduler tick (30 s) in `scheduler.rs` that takes due jobs, applies the gate, and reschedules gated jobs by 5 minutes and drops them after 24 hours
- [ ] 4.8 Add the expiry of `ai_results` rows to the existing expiry tick

## 5. Daemon dispatch and logging

- [ ] 5.1 Add dispatch arms for `AiSuggestionGet` (cache lookup, enqueue missing jobs, return `ready` / `pending` / `none` / `unavailable`)
- [ ] 5.2 Add the dispatch arm for `AiRewrite` (front of queue, no gate, no cache, no silent retry)
- [ ] 5.3 Add the dispatch arm for `AiMemoriesList` returning ids and titles only
- [ ] 5.4 Enqueue draft and TODO jobs when a thread from an `allowed_senders` match is opened (use the existing sender-match helper)
- [ ] 5.5 Add the operational log line per call (component `apfel`, tool, short thread hash, duration, outcome, queue depth, load, free MB) and a test that the line contains no content
- [ ] 5.6 Add the audit record `ai.call` per call with the same short hash and outcome; check the existing hash-chain test still passes
- [ ] 5.7 Log memory parse warnings with file name and reason only

## 6. TUI

- [ ] 6.1 Add `suggestion` to the reply form state in `compose.rs` and render it dimmed after the cursor in `ui.rs`
- [ ] 6.2 Poll `AiSuggestionGet` when the reply form opens; show the suggestion when `ready`
- [ ] 6.3 Accept on `Right` at end of body or on `Tab` (newline first if non-empty); dismiss on any typed character; `Tab` moves focus again when no suggestion is shown
- [ ] 6.4 Add the rewrite popup (five modes, custom instruction line) bound to `Ctrl-r`, with spinner, preview, confirm and `u` undo
- [ ] 6.5 Handle rewrite outcomes (`refused`, `timeout`, `unavailable`, empty draft) with one-line messages
- [ ] 6.6 Add `t` on a thread in `app.rs` that requests the TODO suggestion and opens the existing TODO popup prefilled, or empty with "no task suggested" / "no suggestion yet"
- [ ] 6.7 Add TUI tests for accept, dismiss, undo and the prefill mapping, following the existing `compose.rs` tests

## 7. Verification and docs

- [ ] 7.1 Run `cargo test --workspace`, `cargo clippy --workspace`, and `cargo fmt --check`
- [ ] 7.2 Measure on this Mac: core count, typical load, free memory during use, `apfel --count-tokens` on real threads, and calibrate `APFEL_MAX_LOAD_PER_CORE`, `APFEL_MIN_FREE_MB`, `APFEL_PROMPT_TOKEN_BUDGET`; record the values in the design
- [ ] 7.3 Manual smoke check: open an approved-sender thread, see ghost text, accept with Right, run each rewrite mode, press `t`, and confirm the operational log and audit show only numbers and hashed ids
- [ ] 7.4 Add an "AI helpers (apfel)" section to README covering setup, memories format, the settings file, and the logging guarantees
- [ ] 7.5 Restart `hasted` after installing the new binaries and say so in the hand-off
