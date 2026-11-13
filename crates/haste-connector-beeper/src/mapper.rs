//! Mapping Beeper messages into the unified model, and networks to sources.

use haste_core::model::{Message, Source};
use haste_core::time::parse_rfc3339;

use crate::consts::{ATTACHMENT_LABEL, BEEPER_BODY_MAX_BYTES, TRUNCATED_MARKER};
use crate::wire::{sort_key_string, WireMessage};

/// Which network a Beeper account is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeeperSource {
    Whatsapp,
    Messenger,
    Imessage,
}

impl BeeperSource {
    pub const ALL: [Self; 3] = [Self::Whatsapp, Self::Messenger, Self::Imessage];

    #[must_use]
    pub fn source(self) -> Source {
        match self {
            Self::Whatsapp => Source::Whatsapp,
            Self::Messenger => Source::Messenger,
            Self::Imessage => Source::Imessage,
        }
    }

    /// The source for a Beeper `network` string, or `None` for an unmapped network (such an
    /// account is refused, never guessed). The exact strings come from the spike; matching is
    /// by lowercase substring until then.
    #[must_use]
    pub fn from_network(network: &str) -> Option<Self> {
        let n = network.to_lowercase();
        if n.contains("whatsapp") {
            Some(Self::Whatsapp)
        } else if n.contains("imessage") {
            Some(Self::Imessage)
        } else if n.contains("messenger") || n.contains("facebook") {
            Some(Self::Messenger)
        } else {
            None
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Whatsapp => "whatsapp",
            Self::Messenger => "messenger",
            Self::Imessage => "imessage",
        }
    }
}

/// Cut text to `max` bytes at a character boundary, adding a marker when something was cut.
#[must_use]
pub fn truncate_body(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED_MARKER}", &text[..end])
}

/// A mapped message and the sort key that places it in its chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub message: Message,
    pub sort_key: String,
    pub timestamp: i64,
}

/// Who sent it, for display: the sender's name, else the sender id, else nothing. Control
/// characters are dropped and the name is bounded.
fn sender_label(wire: &WireMessage) -> Option<String> {
    let name: String = wire
        .sender_name
        .as_deref()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let name = name.trim();
    if !name.is_empty() {
        return Some(name.to_owned());
    }
    (!wire.sender_id.is_empty()).then(|| wire.sender_id.clone())
}

/// Map one wire message. `None` when it cannot be placed (no usable timestamp or sort key): such a
/// message is skipped and counted, never guessed. A message with attachments stores one line per
/// file (name, type, size and Beeper's asset id, see [`haste_core::attachment`]); a media message
/// Beeper gives no list for stores a fixed label.
#[must_use]
pub fn map_message(chat_id: &str, source: &Source, wire: &WireMessage) -> Option<Mapped> {
    // Reactions, notices (state events), deleted and hidden messages are not conversation text
    // (spike 2026-11-10; message types per the local OpenAPI spec).
    let reaction = wire
        .kind
        .as_deref()
        .is_some_and(|k| k.eq_ignore_ascii_case("reaction") || k.eq_ignore_ascii_case("notice"));
    if reaction || wire.is_deleted || wire.is_hidden {
        return None;
    }
    let timestamp = parse_rfc3339(&wire.timestamp)?;
    let sort_key = wire.sort_key.as_ref().and_then(sort_key_string)?;
    let is_text = wire
        .kind
        .as_deref()
        .is_none_or(|k| k.eq_ignore_ascii_case("text") || k.is_empty());
    let mut body = match (&wire.text, is_text) {
        (Some(t), true) if !t.is_empty() => truncate_body(t, BEEPER_BODY_MAX_BYTES),
        _ if !wire.attachments.is_empty() => String::new(),
        _ => ATTACHMENT_LABEL.to_owned(),
    };
    for a in wire.attachments.iter().filter(|a| !a.id.is_empty()) {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&haste_core::attachment::format_line(
            a.file_name.as_deref().unwrap_or("attachment"),
            a.mime_type.as_deref().unwrap_or(""),
            // sizes are whole bytes; a missing or odd one is shown as 0
            a.file_size.map_or(0, |s| {
                if s.is_finite() && s > 0.0 {
                    s as u64
                } else {
                    0
                }
            }),
            Some(&a.id),
        ));
    }
    if body.is_empty() {
        body = ATTACHMENT_LABEL.to_owned();
    }
    Some(Mapped {
        message: Message {
            id: String::new(),
            // the store resolves the chat id to its thread, as for the other connectors
            thread_id: chat_id.to_owned(),
            source: source.clone(),
            external_id: wire.id.clone(),
            sender_person_id: None,
            // the person's name reads better than an id; the id is the fallback
            sender_identifier: sender_label(wire),
            body_text: body,
            sent_at: timestamp,
            is_own: wire.is_sender,
        },
        sort_key,
        timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(json: &str) -> WireMessage {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_sender_name_is_shown_instead_of_the_id_and_the_id_is_the_fallback() {
        let named = map_message("c1", &Source::Whatsapp, &wire(r#"{"id":"m1","senderID":"u1","senderName":"  Ana\u0007 Kiss ","text":"hi","timestamp":"2026-11-09T10:00:00Z","sortKey":"005","isSender":false}"#)).unwrap();
        assert_eq!(named.message.sender_identifier.as_deref(), Some("Ana Kiss"));
        let nameless = map_message("c1", &Source::Whatsapp, &wire(r#"{"id":"m2","senderID":"u1","senderName":"","text":"hi","timestamp":"2026-11-09T10:00:00Z","sortKey":"006"}"#)).unwrap();
        assert_eq!(nameless.message.sender_identifier.as_deref(), Some("u1"));
    }

    #[test]
    fn networks_map_to_sources_and_unknown_networks_to_nothing() {
        assert_eq!(
            BeeperSource::from_network("whatsapp"),
            Some(BeeperSource::Whatsapp)
        );
        assert_eq!(
            BeeperSource::from_network("WhatsApp"),
            Some(BeeperSource::Whatsapp)
        );
        assert_eq!(
            BeeperSource::from_network("facebookgo"),
            Some(BeeperSource::Messenger)
        );
        assert_eq!(
            BeeperSource::from_network("facebook"),
            Some(BeeperSource::Messenger)
        );
        assert_eq!(
            BeeperSource::from_network("messenger"),
            Some(BeeperSource::Messenger)
        );
        assert_eq!(
            BeeperSource::from_network("imessage"),
            Some(BeeperSource::Imessage)
        );
        assert_eq!(BeeperSource::from_network("signal"), None);
        assert_eq!(BeeperSource::from_network(""), None);
    }

    #[test]
    fn bodies_are_cut_at_a_character_boundary_with_a_marker() {
        assert_eq!(truncate_body("short", 100), "short");
        let cut = truncate_body(&"é".repeat(50), 11); // 2 bytes each: 11 falls inside a character
        assert!(cut.ends_with(TRUNCATED_MARKER));
        assert!(cut
            .trim_end_matches(TRUNCATED_MARKER)
            .chars()
            .all(|c| c == 'é'));
        assert!(cut.len() <= 11 + TRUNCATED_MARKER.len());
    }

    #[test]
    fn listed_attachments_become_lines_with_the_asset_to_fetch() {
        let m = map_message(
            "c1",
            &Source::Whatsapp,
            &wire(
                r#"{"id":"m1","senderID":"u1","text":"here","timestamp":"2026-11-09T10:00:00Z","sortKey":"005",
                    "attachments":[{"id":"mxc://beeper.local/abc","fileName":"menu.pdf","mimeType":"application/pdf","fileSize":2048}]}"#,
            ),
        )
        .unwrap();
        let lines = haste_core::attachment::lines_of(&m.message.body_text);
        assert!(m.message.body_text.starts_with("here\n"));
        assert_eq!(
            (
                lines[0].name.as_str(),
                lines[0].bytes,
                lines[0].reference.as_deref()
            ),
            ("menu.pdf", 2048, Some("mxc://beeper.local/abc"))
        );
        // a media message with a list but no text is just the lines
        let media = map_message(
            "c1",
            &Source::Whatsapp,
            &wire(
                r#"{"id":"m2","senderID":"u1","type":"IMAGE","timestamp":"2026-11-09T10:00:00Z","sortKey":"006",
                    "attachments":[{"id":"mxc://x/y","fileName":"a.png"}]}"#,
            ),
        )
        .unwrap();
        assert!(media.message.body_text.starts_with("[attachment: a.png"));
    }

    #[test]
    fn text_maps_and_non_text_becomes_a_fixed_label_never_a_name() {
        let ok = map_message("c1", &Source::Whatsapp, &wire(r#"{"id":"m1","senderID":"u1","text":"hi there","timestamp":"2026-11-09T10:00:00Z","sortKey":"005"}"#)).unwrap();
        assert_eq!(ok.message.body_text, "hi there");
        assert_eq!(
            (
                ok.message.thread_id.as_str(),
                ok.message.external_id.as_str()
            ),
            ("c1", "m1")
        );
        assert_eq!(ok.message.sent_at, 1_794_218_400);
        assert_eq!(ok.sort_key, "005");
        let media = map_message("c1", &Source::Whatsapp, &wire(r#"{"id":"m2","text":"holiday.jpg","type":"image","timestamp":"2026-11-09T10:00:00Z","sortKey":6}"#)).unwrap();
        assert_eq!(media.message.body_text, ATTACHMENT_LABEL);
        assert_eq!(media.sort_key, "6");
        let empty = map_message(
            "c1",
            &Source::Whatsapp,
            &wire(r#"{"id":"m3","timestamp":"2026-11-09T10:00:00Z","sortKey":"7"}"#),
        )
        .unwrap();
        assert_eq!(empty.message.body_text, ATTACHMENT_LABEL);
    }

    #[test]
    fn reactions_deleted_and_hidden_messages_are_skipped() {
        for json in [
            r#"{"id":"m","type":"REACTION","text":"x","timestamp":"2026-11-09T10:00:00Z","sortKey":"1"}"#,
            r#"{"id":"m","type":"TEXT","text":"x","isDeleted":true,"timestamp":"2026-11-09T10:00:00Z","sortKey":"1"}"#,
            r#"{"id":"m","type":"TEXT","text":"x","isHidden":true,"timestamp":"2026-11-09T10:00:00Z","sortKey":"1"}"#,
        ] {
            assert!(
                map_message("c", &Source::Whatsapp, &wire(json)).is_none(),
                "{json}"
            );
        }
        let plain = r#"{"id":"m","type":"TEXT","text":"x","timestamp":"2026-11-09T10:00:00Z","sortKey":"1"}"#;
        assert!(map_message("c", &Source::Whatsapp, &wire(plain)).is_some());
    }

    #[test]
    fn a_message_that_cannot_be_placed_is_skipped_not_guessed() {
        for json in [
            r#"{"id":"m","text":"x","timestamp":"not a time","sortKey":"1"}"#,
            r#"{"id":"m","text":"x","timestamp":"2026-11-09T10:00:00Z"}"#,
            r#"{"id":"m","text":"x","timestamp":"2026-11-09T10:00:00Z","sortKey":null}"#,
        ] {
            assert!(
                map_message("c", &Source::Whatsapp, &wire(json)).is_none(),
                "{json}"
            );
        }
    }
}
