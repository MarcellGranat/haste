//! Colours for the calendars on the Calendar tab.
//!
//! Every calendar gets a colour of its own: one picked from a built-in palette by the calendar's
//! id (the same colour every time), or the one written in `calendar_colors.json` in the data
//! folder. The file is a flat object from a calendar to a colour:
//!
//! ```json
//! { "Work": "#f38ba8", "me@example.org": "teal", "local": "sky" }
//! ```
//!
//! A calendar is named by its name or its id (capitals do not matter); `local` is the events
//! that live only in haste. A colour is `#rrggbb`, `#rgb` or a palette name.

use std::collections::HashMap;
use std::path::Path;

use ratatui::style::Color;

use crate::theme;

/// The file, in the data folder.
pub const FILE: &str = "calendar_colors.json";

/// The names a colour may be written as (`rose` is a vivid one of its own).
pub const NAMES: [(&str, Color); 15] = [
    ("rosewater", theme::ROSEWATER),
    ("rose", Color::Rgb(0xff, 0x2e, 0x79)),
    ("flamingo", theme::FLAMINGO),
    ("pink", theme::PINK),
    ("mauve", theme::MAUVE),
    ("red", theme::RED),
    ("maroon", theme::MAROON),
    ("peach", theme::PEACH),
    ("yellow", theme::YELLOW),
    ("green", theme::GREEN),
    ("teal", theme::TEAL),
    ("sky", theme::SKY),
    ("sapphire", theme::SAPPHIRE),
    ("blue", theme::BLUE),
    ("lavender", theme::LAVENDER),
];

/// What a calendar without a colour of its own picks from: not mauve (the cursor) and not red
/// (the line for the time now), so they keep meaning one thing.
const PALETTE: [Color; 11] = [
    theme::BLUE,
    theme::GREEN,
    theme::PEACH,
    theme::PINK,
    theme::TEAL,
    theme::YELLOW,
    theme::SKY,
    theme::FLAMINGO,
    theme::LAVENDER,
    theme::SAPPHIRE,
    theme::ROSEWATER,
];

/// The colour of the events that live only in haste, when the file does not say.
const LOCAL: Color = theme::SKY;

/// Parse `#rrggbb`, `#rgb` or a palette name.
#[must_use]
pub fn parse_colour(text: &str) -> Option<Color> {
    let text = text.trim().to_lowercase();
    if let Some(hex) = text.strip_prefix('#') {
        let digit = |c: char| c.to_digit(16).and_then(|d| u8::try_from(d).ok());
        let d: Vec<u8> = hex.chars().map(digit).collect::<Option<_>>()?;
        return match d.as_slice() {
            [r, g, b] => Some(Color::Rgb(r * 17, g * 17, b * 17)),
            [r1, r2, g1, g2, b1, b2] => Some(Color::Rgb(r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2)),
            _ => None,
        };
    }
    NAMES.iter().find(|(n, _)| *n == text).map(|(_, c)| *c)
}

/// A colour mixed with the page colour: `weight` of 100 is the colour itself, 0 the page.
#[must_use]
pub fn tint(colour: Color, weight: u16) -> Color {
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => (u16::from(r), u16::from(g), u16::from(b)),
        _ => (0x1e, 0x1e, 0x2e),
    };
    let (c, b) = (rgb(colour), rgb(theme::BASE));
    let mix = |c: u16, b: u16| u8::try_from((c * weight + b * (100 - weight)) / 100).unwrap_or(0);
    Color::Rgb(mix(c.0, b.0), mix(c.1, b.1), mix(c.2, b.2))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalendarColors {
    /// The colour written for a calendar, by its lowercased name or id.
    by_key: HashMap<String, Color>,
    /// How many entries of the file were not understood (and so left out).
    pub ignored: usize,
}

impl CalendarColors {
    /// The colours in `calendar_colors.json` in `dir`. A missing file is no colours of one's
    /// own; a file that is not a JSON object counts as one ignored entry.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        match std::fs::read_to_string(dir.join(FILE)) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut colors = Self::default();
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text)
        else {
            colors.ignored = 1;
            return colors;
        };
        for (key, value) in map {
            match value.as_str().and_then(parse_colour) {
                Some(c) => {
                    colors.by_key.insert(key.trim().to_lowercase(), c);
                }
                None => colors.ignored += 1,
            }
        }
        colors
    }

    /// The colour of a calendar with this id and name: the one written for its name, else for
    /// its id, else one from the palette that always comes out the same for the id.
    #[must_use]
    pub fn colour(&self, id: &str, name: &str) -> Color {
        let written = |key: &str| self.by_key.get(&key.trim().to_lowercase()).copied();
        if let Some(c) = written(name).or_else(|| written(id)) {
            return c;
        }
        if id == haste_core::model::LOCAL_CALENDAR {
            return LOCAL;
        }
        // FNV-1a: the same id is the same colour on every machine and every start
        let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        PALETTE[usize::try_from(hash % PALETTE.len() as u64).unwrap_or(0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_hex_or_palette_names() {
        assert_eq!(parse_colour("#f38ba8"), Some(Color::Rgb(0xf3, 0x8b, 0xa8)));
        assert_eq!(parse_colour("#F00"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_colour(" Peach "), Some(theme::PEACH));
        for bad in ["", "#12", "#gggggg", "orange", "#12345"] {
            assert_eq!(parse_colour(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_calendar_is_found_by_its_name_or_its_id_ignoring_capitals() {
        let c = CalendarColors::parse(
            r##"{ "Work": "#112233", "Me@Example.org": "teal", "local": "pink" }"##,
        );
        assert_eq!(c.colour("id-1", "work"), Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(c.colour("me@example.org", ""), theme::TEAL);
        assert_eq!(c.colour("local", ""), theme::PINK);
        // the name wins over the id
        let both = CalendarColors::parse(r#"{ "a": "red", "A name": "green" }"#);
        assert_eq!(both.colour("a", "A name"), theme::GREEN);
        assert_eq!(c.ignored, 0);
    }

    #[test]
    fn what_the_file_does_not_say_is_picked_from_the_palette_the_same_way_every_time() {
        let none = CalendarColors::default();
        let first = none.colour("calendar-one", "");
        assert_eq!(first, none.colour("calendar-one", "Anything"), "by id");
        assert!(PALETTE.contains(&first));
        assert_eq!(none.colour("local", ""), LOCAL);
        // a few different calendars do not all get the same colour
        let picked: std::collections::HashSet<String> = (0..8)
            .map(|i| format!("{:?}", none.colour(&format!("cal-{i}@group"), "")))
            .collect();
        assert!(picked.len() >= 3, "{picked:?}");
        // neither the cursor's colour nor the time line's
        assert!(!PALETTE.contains(&theme::MAUVE) && !PALETTE.contains(&theme::RED));
    }

    #[test]
    fn bad_entries_are_counted_and_left_out_and_a_bad_file_changes_nothing() {
        let c = CalendarColors::parse(r#"{ "ok": "blue", "bad": "nope", "num": 3 }"#);
        assert_eq!(c.ignored, 2);
        assert_eq!(c.colour("ok", ""), theme::BLUE);
        assert_eq!(CalendarColors::parse("not json").ignored, 1);
        assert_eq!(CalendarColors::parse("[1]").ignored, 1);
        let dir = std::env::temp_dir().join("haste-no-such-colours");
        assert_eq!(CalendarColors::load(&dir), CalendarColors::default());
    }

    #[test]
    fn a_tint_is_between_the_colour_and_the_page() {
        assert_eq!(tint(theme::PEACH, 100), theme::PEACH);
        assert_eq!(tint(theme::PEACH, 0), theme::BASE);
        let Color::Rgb(r, _, _) = tint(Color::Rgb(200, 0, 0), 50) else {
            panic!();
        };
        assert!((100..120).contains(&r), "{r}");
    }
}
