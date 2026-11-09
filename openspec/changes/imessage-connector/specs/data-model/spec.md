## ADDED Requirements

### Requirement: iMessage threads and messages use the existing model
iMessage chats SHALL be stored as `Thread` with `source = imessage` and messages as `Message` in that thread, without a new record type.

#### Scenario: iMessage chat stored as thread
- **WHEN** an iMessage chat is synced
- **THEN** it is stored as a `Thread` with `source = imessage`
