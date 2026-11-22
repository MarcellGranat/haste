## ADDED Requirements

### Requirement: Structured TODO suggestion for approved senders
For a thread from an approved sender, the daemon SHALL run a background TODO job that asks `apfel` for a JSON answer matching the schema: `has_task` (boolean), `title` (at most 120 characters), `notes` (at most 500 characters), `due` (a `YYYY-MM-DD` date or null), `priority` (`none`, `low`, `medium` or `high`). The job context SHALL be the latest inbound message and at most 4 earlier messages, plus matching memories.

#### Scenario: Mail contains a request
- **WHEN** the latest message asks the user to send a document by Friday
- **THEN** the stored suggestion has `has_task` true, a title, and a due date in the coming Friday

#### Scenario: Newsletter
- **WHEN** the mail contains no action for the user
- **THEN** the stored suggestion has `has_task` false

#### Scenario: Invalid date
- **WHEN** the model returns a due value that is not a valid date
- **THEN** the due value is treated as null and the rest of the suggestion is kept

### Requirement: Suggestion never creates a TODO by itself
A TODO suggestion SHALL NOT create, change or complete any TODO, reminder or task. Only the user's save in the TODO popup SHALL create a TODO, through the existing human save path.

#### Scenario: Background job finishes
- **WHEN** a TODO suggestion is stored
- **THEN** no reminder or TODO record is created

### Requirement: Pressing t prefills the TODO popup
Pressing `t` on a thread SHALL request the stored TODO suggestion. When `has_task` is true, the existing TODO popup SHALL open with title, notes, due date and priority filled from the suggestion, and every field SHALL remain editable. When `has_task` is false, or no suggestion exists, the popup SHALL open empty and show the note "no task suggested" (or "no suggestion yet" when the job has not run).

#### Scenario: Suggestion with a task
- **WHEN** the user presses `t` on a thread with a task suggestion
- **THEN** the TODO popup opens prefilled and the user can edit each field before saving

#### Scenario: No task suggested
- **WHEN** the user presses `t` and `has_task` is false
- **THEN** the popup opens empty with the note "no task suggested"

#### Scenario: Save
- **WHEN** the user saves the popup
- **THEN** the TODO is created with the values in the popup, not with the suggestion's original values

### Requirement: Suggestion is only shown on request
The TODO suggestion SHALL NOT be displayed in any list, badge or notification. It is shown only inside the TODO popup opened with `t`.

#### Scenario: Inbox list
- **WHEN** a thread with a task suggestion appears in the Today list
- **THEN** the list shows no suggestion marker
