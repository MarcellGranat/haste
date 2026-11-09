# Local patch to imap-proto 0.10.2

Upstream source (MIT/Apache-2.0) with one change in `src/body_structure.rs`, `body_encoding`:
a `NIL` content-transfer-encoding is accepted and read as `7BIT`.

Why: iCloud Mail reports `NIL` for a part that has no `Content-Transfer-Encoding` header, and the
stock parser rejects the whole FETCH response, which stalled the iCloud connector's sync.

Remove this directory and the `[patch.crates-io]` entry in the workspace `Cargo.toml` once an
upstream release accepts `NIL` there.
