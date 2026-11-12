## ADDED Requirements

### Requirement: Filters file in the haste data folder
Preset filters SHALL be stored as named rules in `filters.json` in the haste data folder. Each filter SHALL have a name and a rule over the keys `sender`, `label`, `source` and `unread`.

#### Scenario: Valid file loads
- **WHEN** `filters.json` contains two named filters using only the allowed keys
- **THEN** both filters are available for cycling

### Requirement: Unknown keys are rejected
The filter loader SHALL reject any filter that contains a key other than `sender`, `label`, `source` or `unread`. Rejection SHALL name the unknown key and the filter, and SHALL NOT apply any filter from the file.

#### Scenario: Unknown key
- **WHEN** a filter contains the key `body`
- **THEN** loading fails with an error naming `body` and the filter, and the previously active filter set remains in effect

### Requirement: Key cycles filters
A dedicated key SHALL cycle the active filter through the defined filters in file order, and then back to the default filter.

#### Scenario: Cycle order
- **WHEN** the user presses the filter key three times with filters A and B defined
- **THEN** the active filter becomes A, then B, then the default filter

### Requirement: Default filter
The default filter SHALL match every non-archived thread and SHALL always be available as the last step of the cycle.

#### Scenario: Default always present
- **WHEN** `filters.json` defines no filters
- **THEN** the cycle contains only the default filter

### Requirement: Filter file is read-only to the TUI
The TUI SHALL NOT write `filters.json`. Cycling a filter SHALL change only the in-memory active filter.

#### Scenario: Cycle does not write
- **WHEN** the user cycles the filter
- **THEN** the modification time of `filters.json` is unchanged
