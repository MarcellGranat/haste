## ADDED Requirements

### Requirement: Beeper chat threads carry project tags set by the human
Beeper chat threads SHALL carry project tags the same way as other threads: the human sets them, herdr workspace labels are the candidates, and agents and sync never set or change them.

#### Scenario: Sync keeps human tags
- **WHEN** a selected chat's name changes upstream
- **THEN** the thread's title is updated and its project tags are unchanged

#### Scenario: Agent cannot tag
- **WHEN** an agent requests a tag change on a Beeper thread
- **THEN** the request is denied and no tag changes

### Requirement: Machine filter applies to Beeper threads
A machine principal SHALL see a Beeper thread only when one of its tags is a workspace on that machine. The exhaustive per-principal scope test SHALL include Beeper threads.

#### Scenario: Tagged chat visible on its machine only
- **WHEN** a Beeper thread is tagged `haste` and only machine `m1` has a `haste` workspace
- **THEN** machine `m1` sees the thread and another machine does not
