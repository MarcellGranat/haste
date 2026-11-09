## ADDED Requirements

### Requirement: Daemon runs as a launchd agent with a private Unix socket
The `hasted` daemon SHALL run as a launchd user agent and SHALL accept client connections only on a Unix domain socket located in a directory with mode `0700`. The daemon MUST NOT listen on any TCP or network address.

#### Scenario: Socket directory is private
- **WHEN** the daemon starts for the first time
- **THEN** it creates the socket directory with mode `0700` and creates the socket inside it

#### Scenario: Existing socket directory with loose permissions
- **WHEN** the daemon starts and the socket directory exists with any permission bits beyond `0700`
- **THEN** the daemon refuses to start and logs an error naming the directory

#### Scenario: No network listener
- **WHEN** the daemon is running
- **THEN** no TCP or UDP socket is bound by the daemon process

### Requirement: Thin clients communicate only through the daemon
The TUI shell and the `haste` CLI SHALL NOT open the SQLite database or write audit files directly. All reads and writes SHALL go through the daemon's request protocol.

#### Scenario: Client cannot bypass daemon
- **WHEN** a client needs to change triage state
- **THEN** it sends a request over the socket and does not open the database file

### Requirement: Length-prefixed JSON request protocol
The protocol SHALL frame each message with a 4-byte big-endian length prefix followed by a UTF-8 JSON body. Every request SHALL carry a protocol version and a token. Requests with an unknown protocol version SHALL be rejected with a typed error.

#### Scenario: Unknown protocol version
- **WHEN** a client sends a request with a protocol version the daemon does not support
- **THEN** the daemon responds with an `unsupported_version` error and performs no action

#### Scenario: Oversized frame
- **WHEN** a client announces a frame larger than the configured maximum
- **THEN** the daemon closes the connection and records the event in the audit log

### Requirement: Store lifecycle and migrations
The daemon SHALL open the SQLite store in WAL mode, SHALL apply forward-only migrations on startup tracked by `user_version`, and SHALL refuse to start if the stored schema version is newer than the binary supports.

#### Scenario: Newer schema than binary
- **WHEN** the daemon starts and the database `user_version` is greater than the highest migration it knows
- **THEN** it refuses to start and reports the mismatch

### Requirement: Streaming subscription for clients
The protocol SHALL allow a client to subscribe to change notifications for threads, proposals, and approvals so that the TUI can refresh without polling.

#### Scenario: Subscribed client receives proposal change
- **WHEN** a client is subscribed to proposal changes and a proposal transitions state
- **THEN** the daemon pushes an event containing the proposal ID and new state to that client
