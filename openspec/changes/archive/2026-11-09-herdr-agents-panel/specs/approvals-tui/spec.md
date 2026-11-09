## ADDED Requirements

### Requirement: Blocked agent group in approvals
The approvals view SHALL show a `Blocked agents` group alongside the proposal list. The group SHALL list agents whose current status is `blocked`, with machine label, workspace label and agent name, oldest first. The group SHALL be collapsible like the per-agent groups, SHALL update without manual refresh, and SHALL be read-only.

#### Scenario: Group shows blocked agents
- **WHEN** two agents are `blocked`, one on `local` and one on `m1`
- **THEN** the Blocked agents group lists both with their machine and workspace

#### Scenario: Group empty
- **WHEN** no agent is `blocked`
- **THEN** the group header shows a count of zero and no rows

#### Scenario: Group is read-only
- **WHEN** the user selects an entry in the Blocked agents group
- **THEN** only machine, workspace and agent name are shown and no action is offered

### Requirement: Blocked group is separate from batch approval
Blocked agent entries SHALL NOT be selectable for batch approval, reject, or send, and SHALL NOT count toward the batch confirmation count.

#### Scenario: Batch excludes blocked agents
- **WHEN** the user marks a blocked agent entry and a proposal and confirms batch approval
- **THEN** only the proposal is approved and the confirmation count is one

### Requirement: Group and filter proposals by project
The approvals view SHALL let the user switch between grouping proposals by agent and grouping them by project, and the existing filter SHALL also match a proposal's project. Proposals without a project SHALL be listed under a group named `no project`. The project SHALL be shown on each row.

#### Scenario: Group by project
- **WHEN** the user switches to project grouping with proposals from workspaces `haste` and `notes`
- **THEN** the view shows a `haste` group and a `notes` group, each with its proposals

#### Scenario: Proposal without a project
- **WHEN** a proposal has no project
- **THEN** it appears under `no project` in project grouping

#### Scenario: Filter by project
- **WHEN** the user filters for `haste`
- **THEN** only proposals whose project matches are listed
