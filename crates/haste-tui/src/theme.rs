//! Catppuccin Mocha. Every colour in the TUI comes from here: widgets use the semantic aliases
//! (or a palette constant) and never write an `Rgb` literal of their own.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};

pub const ROSEWATER: Color = Color::Rgb(0xf5, 0xe0, 0xdc);
pub const FLAMINGO: Color = Color::Rgb(0xf2, 0xcd, 0xcd);
pub const PINK: Color = Color::Rgb(0xf5, 0xc2, 0xe7);
pub const MAUVE: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
pub const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
pub const MAROON: Color = Color::Rgb(0xeb, 0xa0, 0xac);
pub const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);
pub const YELLOW: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
pub const GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
pub const TEAL: Color = Color::Rgb(0x94, 0xe2, 0xd5);
pub const SKY: Color = Color::Rgb(0x89, 0xdc, 0xeb);
pub const SAPPHIRE: Color = Color::Rgb(0x74, 0xc7, 0xec);
pub const BLUE: Color = Color::Rgb(0x89, 0xb4, 0xfa);
pub const LAVENDER: Color = Color::Rgb(0xb4, 0xbe, 0xfe);
pub const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
pub const SUBTEXT1: Color = Color::Rgb(0xba, 0xc2, 0xde);
pub const SUBTEXT0: Color = Color::Rgb(0xa6, 0xad, 0xc8);
pub const OVERLAY2: Color = Color::Rgb(0x93, 0x99, 0xb2);
pub const OVERLAY1: Color = Color::Rgb(0x7f, 0x84, 0x9c);
pub const OVERLAY0: Color = Color::Rgb(0x6c, 0x70, 0x86);
pub const SURFACE2: Color = Color::Rgb(0x58, 0x5b, 0x70);
pub const SURFACE1: Color = Color::Rgb(0x45, 0x47, 0x5a);
pub const SURFACE0: Color = Color::Rgb(0x31, 0x32, 0x44);
pub const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
pub const MANTLE: Color = Color::Rgb(0x18, 0x18, 0x25);
pub const CRUST: Color = Color::Rgb(0x11, 0x11, 0x1b);

// Semantic aliases.
pub const BORDER: Color = SURFACE2;
pub const BORDER_ACTIVE: Color = MAUVE;
pub const SELECTED_BG: Color = SURFACE0;
pub const HINT_KEY: Color = PEACH;
pub const HINT_TEXT: Color = OVERLAY1;
pub const UNREAD: Color = TEXT;
pub const PRIORITY: Color = YELLOW;
pub const MUTED: Color = OVERLAY0;
pub const WARNING: Color = YELLOW;
pub const DANGER: Color = RED;
pub const OK: Color = GREEN;

/// Colour that marks a channel (source) in lists.
#[must_use]
pub fn channel_colour(source: &str) -> Color {
    match source {
        "gmail" => RED,
        "icloud" => SAPPHIRE,
        "whatsapp" => GREEN,
        "messenger" => BLUE,
        "imessage" => TEAL,
        _ => SUBTEXT0,
    }
}

#[must_use]
pub fn text() -> Style {
    Style::new().fg(TEXT).bg(BASE)
}

#[must_use]
pub fn muted() -> Style {
    Style::new().fg(MUTED).bg(BASE)
}

#[must_use]
pub fn selected() -> Style {
    Style::new()
        .bg(SELECTED_BG)
        .fg(TEXT)
        .add_modifier(Modifier::BOLD)
}

/// A rounded panel with a titled border.
#[must_use]
pub fn panel(title: &str) -> Block<'_> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER).bg(BASE))
        .style(text())
        .title_style(Style::new().fg(LAVENDER).add_modifier(Modifier::BOLD))
        .title(title)
}

/// A picture's own pixel colour (the one place colours do not come from the palette).
#[must_use]
pub fn pixel(rgb: [u8; 3]) -> Color {
    Color::Rgb(rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_is_catppuccin_mocha() {
        assert_eq!(BASE, Color::Rgb(0x1e, 0x1e, 0x2e));
        assert_eq!(MAUVE, Color::Rgb(0xcb, 0xa6, 0xf7));
        assert_eq!(BORDER_ACTIVE, MAUVE);
    }

    #[test]
    fn every_channel_has_a_colour() {
        for c in ["gmail", "icloud", "whatsapp", "messenger", "imessage"] {
            assert_ne!(channel_colour(c), SUBTEXT0, "{c}");
        }
    }
}
