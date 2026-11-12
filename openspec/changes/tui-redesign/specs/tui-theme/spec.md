## ADDED Requirements

### Requirement: Catppuccin Mocha palette in one module
All TUI colours SHALL be defined as named constants of the Catppuccin Mocha palette in a single theme module. No other module SHALL contain a literal colour value.

#### Scenario: Single source of colour
- **WHEN** the TUI source is searched for colour literals outside the theme module
- **THEN** no `Color::Rgb` or equivalent literal is found

#### Scenario: Palette value
- **WHEN** the theme module's base background constant is read
- **THEN** it equals the Catppuccin Mocha Base value

### Requirement: Rounded borders
All bordered panels SHALL use rounded border glyphs.

#### Scenario: Panel border style
- **WHEN** any panel is drawn
- **THEN** its border type is rounded

### Requirement: Semantic colour aliases
The theme module SHALL expose semantic aliases for border, selection, key hint, hint text, unread, and priority, and widgets SHALL use these aliases rather than palette names.

#### Scenario: Unread styling
- **WHEN** a thread is unread
- **THEN** its row uses the unread alias
