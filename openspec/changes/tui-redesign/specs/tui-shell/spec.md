## ADDED Requirements

### Requirement: Three-tab layout
The TUI SHALL present three tabs in this order: Today, Channels, Calendar. The Today tab SHALL be the default tab on start.

#### Scenario: Default tab on start
- **WHEN** haste-tui starts
- **THEN** the Today tab is active

#### Scenario: Switch tabs
- **WHEN** the user presses the tab-switch key
- **THEN** the next tab in the order Today, Channels, Calendar becomes active and the previous tab's selection is preserved

### Requirement: Status bar with key hints
The TUI SHALL render a status bar at the bottom of the screen showing the active tab and the key hints valid for the focused view. The status bar SHALL show the pending-approvals count on every tab.

#### Scenario: Hints follow focus
- **WHEN** the focus moves from the inbox list to the reply prompt
- **THEN** the status bar shows the hints for the reply prompt instead of the list hints

#### Scenario: Pending approvals visible on every tab
- **WHEN** two proposals are pending and the Calendar tab is active
- **THEN** the status bar shows a pending-approvals count of 2

### Requirement: Secondary overlay for existing views
The TUI SHALL reach Approvals, Connectors and Agents through a secondary overlay opened from any tab. Each overlay SHALL keep the behaviour defined for it in its own capability spec.

#### Scenario: Open approvals from Today
- **WHEN** the user opens the approvals overlay while the Today tab is active
- **THEN** the approvals view is shown and closing it returns to the Today tab with its selection unchanged

#### Scenario: Nothing removed
- **WHEN** the user opens each of Approvals, Connectors and Agents in turn
- **THEN** each view lists the same data it listed before this change

### Requirement: Vi-style navigation in every list
Every list in the TUI SHALL support `j` and `k` to move the selection by one item and `g` and `G` to jump to the first and last item.

#### Scenario: Jump to last item
- **WHEN** the user presses `G` in a list of forty threads
- **THEN** the selection is on the fortieth thread

#### Scenario: Empty list
- **WHEN** the user presses `j` in an empty list
- **THEN** the selection stays unset and no error is shown

### Requirement: Effects only through the daemon
The TUI SHALL perform every state change and external effect through daemon requests. The haste-tui crate SHALL NOT depend on connector crates or on `reqwest`.

#### Scenario: Dependency check
- **WHEN** the dependency list of `crates/haste-tui/Cargo.toml` is inspected
- **THEN** it contains no connector crate and no `reqwest` entry
