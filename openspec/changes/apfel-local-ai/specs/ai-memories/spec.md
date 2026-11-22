## ADDED Requirements

### Requirement: Memories are local YAML files
A memory SHALL be a YAML file `memories/<name>.yml` in the haste data directory with the keys `id`, `title`, `enabled`, `channels`, `senders` and `text`. The daemon SHALL read at most 50 memory files, each at most 4 KB, and SHALL skip and log any file that does not parse, naming only the file name and the reason.

#### Scenario: Valid memory
- **WHEN** a file has `id`, `text`, `enabled: true` and a `channels` or `senders` field
- **THEN** it is loaded as an active memory

#### Scenario: Malformed file
- **WHEN** a file is not valid YAML or has no `text`
- **THEN** it is skipped and a warning with the file name and reason is logged

#### Scenario: Too many files
- **WHEN** more than 50 files exist
- **THEN** only the first 50 in name order are loaded and a warning is logged

### Requirement: Trigger rules by channel and by sender
A memory SHALL match a thread when `enabled` is true and at least one of the following holds: the thread's channel is listed in `channels`, or any pattern in `senders` (regular expressions, case-insensitive, same syntax and length limits as the `allowed_senders` file) matches a sender of the thread. A memory with neither field SHALL NOT match any thread.

#### Scenario: Matched by channel
- **WHEN** a memory lists `gmail` in `channels` and the thread is a Gmail thread
- **THEN** the memory matches

#### Scenario: Matched by sender
- **WHEN** a memory has the pattern `kata\s+biro` and a thread has a sender named "Kata Biro"
- **THEN** the memory matches

#### Scenario: Disabled memory
- **WHEN** `enabled` is false
- **THEN** the memory never matches

#### Scenario: Invalid pattern
- **WHEN** a sender pattern does not compile or exceeds the length limit
- **THEN** that pattern is ignored and a warning names the memory id and the line number, never the pattern

### Requirement: Limited memories per call
At most 3 matching memories SHALL be passed to a call, ordered by id, and their combined body text SHALL be capped at 1 KB. Memories SHALL NOT be passed to the model for any thread that does not match them.

#### Scenario: Four memories match
- **WHEN** four memories match a thread
- **THEN** the three lowest ids are passed and the fourth is not

### Requirement: Memory fingerprint
The daemon SHALL compute a fingerprint over the ids and bodies of the memories passed to a call, and SHALL use it in the result cache key, so that editing a memory invalidates cached suggestions for affected threads.

#### Scenario: Memory edited
- **WHEN** the body of a matched memory changes
- **THEN** cached suggestions for threads it matches are not returned again

### Requirement: Memory content stays local and out of logs
Memory text SHALL be sent only to the local `apfel` process and SHALL NOT be sent to any other service, logged, written to the audit trail, or exposed through any request other than as a title list to the human principal.

#### Scenario: Listing for the TUI
- **WHEN** the TUI asks which memories apply to a thread
- **THEN** the daemon returns memory ids and titles only
