//! Gmail JSON to the haste data model. Labels are read, never written.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use haste_core::model::{Message, Source, Thread};

use crate::client::{ApiMessage, ApiPart, ApiThread};
use crate::consts::{BODY_UNAVAILABLE_MARKER, MAX_BODY_BYTES};

/// Labels seen anywhere in the thread. Read-only information; never written back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Labels {
    pub inbox: bool,
    pub unread: bool,
    pub sent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedThread {
    pub thread: Thread,
    pub messages: Vec<Message>,
    pub labels: Labels,
}

fn header<'a>(part: &'a ApiPart, name: &str) -> Option<&'a str> {
    part.headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

/// Split an address header into bare, lowercase addresses.
#[must_use]
pub fn addresses(header: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let (mut in_quotes, mut in_angle) = (false, false);
    let flush = |current: &mut String, out: &mut Vec<String>| {
        let part = current.trim();
        let address = match (part.find('<'), part.rfind('>')) {
            (Some(a), Some(b)) if a < b => part[a + 1..b].trim(),
            _ => part,
        };
        if address.contains('@') {
            out.push(address.to_lowercase());
        }
        current.clear();
    };
    for c in header.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            '<' if !in_quotes => in_angle = true,
            '>' if !in_quotes => in_angle = false,
            ',' if !in_quotes && !in_angle => {
                flush(&mut current, &mut out);
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    flush(&mut current, &mut out);
    out
}

/// Truncate to at most `max` bytes without splitting a UTF-8 character.
#[must_use]
pub fn truncate_utf8(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn part_text(part: &ApiPart, mime: &str) -> Option<String> {
    if part.mime_type.eq_ignore_ascii_case(mime) {
        let data = part.body.data.as_deref()?;
        let bytes = URL_SAFE_NO_PAD.decode(data.trim_end_matches('=')).ok()?;
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    part.parts.iter().find_map(|p| part_text(p, mime))
}

/// The readable body: `text/plain` when there is one, otherwise the `text/html` part reduced to
/// text (nothing is rendered).
fn plain_text(part: &ApiPart) -> Option<String> {
    part_text(part, "text/plain").or_else(|| {
        part_text(part, "text/html")
            .map(|html| haste_core::html::to_text(&html))
            .filter(|text| !text.is_empty())
    })
}

/// One stored line per attachment part (see [`haste_core::attachment`]), carrying Gmail's
/// attachment id so the human can fetch the file later. Only metadata is read now.
fn attachment_lines(part: &ApiPart, out: &mut Vec<String>) {
    if !part.filename.trim().is_empty() {
        out.push(haste_core::attachment::format_line(
            &part.filename,
            &part.mime_type.to_lowercase(),
            part.body.size,
            part.body.attachment_id.as_deref(),
        ));
    }
    for p in &part.parts {
        attachment_lines(p, out);
    }
}

fn map_message(message: &ApiMessage, thread_external_id: &str) -> Message {
    let payload = message.payload.clone().unwrap_or_default();
    let mut body_text = match plain_text(&payload) {
        Some(text) => truncate_utf8(&text, MAX_BODY_BYTES).to_owned(),
        None => format!(
            "{BODY_UNAVAILABLE_MARKER}{}",
            truncate_utf8(&message.snippet, MAX_BODY_BYTES)
        ),
    };
    let mut files = Vec::new();
    attachment_lines(&payload, &mut files);
    for line in files {
        body_text.push('\n');
        body_text.push_str(&line);
    }
    let sender_identifier = header(&payload, "From").and_then(|f| addresses(f).into_iter().next());
    let sent_at = message
        .internal_date
        .as_deref()
        .and_then(|ms| ms.parse::<i64>().ok())
        .map_or(0, |ms| ms / 1000);
    Message {
        // The store assigns the real id; the daemon resolves `thread_id` from the external id.
        id: String::new(),
        thread_id: thread_external_id.to_owned(),
        source: Source::Gmail,
        external_id: message.id.clone(),
        sender_person_id: None,
        sender_identifier,
        body_text,
        sent_at,
        is_own: false,
    }
}

/// Which inbox tabs are loaded. The default is Primary only; `gmail_tabs.json` in the data
/// folder can add the others: `{ "tabs": ["primary", "updates"] }`, or `{ "tabs": "all" }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tabs {
    pub primary: bool,
    pub promotions: bool,
    pub social: bool,
    pub updates: bool,
    pub forums: bool,
}

/// The file with the choice, in the data folder (no secret in it).
pub const TABS_FILE: &str = "gmail_tabs.json";

impl Tabs {
    pub const PRIMARY_ONLY: Self = Self {
        primary: true,
        promotions: false,
        social: false,
        updates: false,
        forums: false,
    };
    pub const ALL: Self = Self {
        primary: true,
        promotions: true,
        social: true,
        updates: true,
        forums: true,
    };

    /// Only Primary: Gmail's own search can then do the filtering.
    #[must_use]
    pub fn is_primary_only(&self) -> bool {
        *self == Self::PRIMARY_ONLY
    }

    /// The tabs a text names (`primary`, `promotions`, `social`, `updates`, `forums`, capitals
    /// do not matter); a name that is not one of them is left out, and an empty choice falls
    /// back to Primary.
    #[must_use]
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Self {
        let mut tabs = Self {
            primary: false,
            ..Self::PRIMARY_ONLY
        };
        for name in names {
            match name.as_ref().trim().to_lowercase().as_str() {
                "primary" => tabs.primary = true,
                "promotions" => tabs.promotions = true,
                "social" => tabs.social = true,
                "updates" => tabs.updates = true,
                "forums" => tabs.forums = true,
                "all" => return Self::ALL,
                _ => {}
            }
        }
        if tabs
            == (Self {
                primary: false,
                ..Self::PRIMARY_ONLY
            })
        {
            Self::PRIMARY_ONLY
        } else {
            tabs
        }
    }

    /// The choice in `gmail_tabs.json` in `dir`; no file, or one that cannot be read, is
    /// Primary only.
    #[must_use]
    pub fn load(dir: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(dir.join(TABS_FILE)) else {
            return Self::PRIMARY_ONLY;
        };
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text)
        else {
            return Self::PRIMARY_ONLY;
        };
        match map.get("tabs") {
            Some(serde_json::Value::String(one)) => Self::from_names(&[one.as_str()]),
            Some(serde_json::Value::Array(list)) => {
                let names: Vec<&str> = list.iter().filter_map(|v| v.as_str()).collect();
                Self::from_names(&names)
            }
            _ => Self::PRIMARY_ONLY,
        }
    }
}

/// Is this thread in the inbox, in one of the tabs that are loaded? True when one of its
/// messages sits in INBOX in such a tab (no category label, or the Personal one, is Primary). Anything else (other
/// folders, spam, archived, tabs left out) is not loaded.
#[must_use]
pub fn is_wanted_inbox(api: &ApiThread, tabs: &Tabs) -> bool {
    api.messages.iter().any(|m| {
        let has = |label: &str| m.label_ids.iter().any(|l| l == label);
        if !has("INBOX") {
            return false;
        }
        let other = has("CATEGORY_PROMOTIONS")
            || has("CATEGORY_SOCIAL")
            || has("CATEGORY_UPDATES")
            || has("CATEGORY_FORUMS");
        // Gmail's "Personal" category is the Primary tab: it counts even beside another one
        (tabs.primary && (has("CATEGORY_PERSONAL") || !other))
            || (tabs.promotions && has("CATEGORY_PROMOTIONS"))
            || (tabs.social && has("CATEGORY_SOCIAL"))
            || (tabs.updates && has("CATEGORY_UPDATES"))
            || (tabs.forums && has("CATEGORY_FORUMS"))
    })
}

/// Is this thread in the default inbox (the Primary tab)?
#[must_use]
pub fn is_primary_inbox(api: &ApiThread) -> bool {
    is_wanted_inbox(api, &Tabs::PRIMARY_ONLY)
}

/// Map a fully fetched Gmail thread. `Message::thread_id` carries the Gmail thread id.
#[must_use]
pub fn map_thread(api: &ApiThread) -> MappedThread {
    let mut participants: Vec<String> = Vec::new();
    let mut labels = Labels::default();
    let mut subject = None;
    let mut last_activity = 0;
    let mut messages = Vec::new();
    for m in &api.messages {
        let payload = m.payload.clone().unwrap_or_default();
        if subject.is_none() {
            subject = header(&payload, "Subject").map(str::to_owned);
        }
        for name in ["From", "To", "Cc"] {
            for a in header(&payload, name).map(addresses).unwrap_or_default() {
                if !participants.contains(&a) {
                    participants.push(a);
                }
            }
        }
        for label in &m.label_ids {
            match label.as_str() {
                "INBOX" => labels.inbox = true,
                "UNREAD" => labels.unread = true,
                "SENT" => labels.sent = true,
                _ => {}
            }
        }
        let mapped = map_message(m, &api.id);
        last_activity = last_activity.max(mapped.sent_at);
        messages.push(mapped);
    }
    MappedThread {
        thread: Thread {
            id: api.id.clone(),
            source: Source::Gmail,
            external_id: api.id.clone(),
            subject: subject.unwrap_or_else(|| "(no subject)".to_owned()),
            participants,
            last_activity_at: last_activity,
            version: 0,
        },
        messages,
        labels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_with(labels: &[&str]) -> ApiThread {
        serde_json::from_value(serde_json::json!({
            "id": "t",
            "messages": [{ "id": "m", "labelIds": labels }]
        }))
        .unwrap()
    }

    #[test]
    fn attachments_become_metadata_lines_and_are_never_fetched() {
        let thread: ApiThread = serde_json::from_value(serde_json::json!({
            "id": "t",
            "messages": [{
                "id": "m",
                "labelIds": ["INBOX"],
                "payload": {
                    "mimeType": "multipart/mixed",
                    "headers": [{ "name": "From", "value": "a@x.org" }],
                    "parts": [
                        { "mimeType": "text/plain", "body": { "data": "SGVsbG8", "size": 5 } },
                        { "mimeType": "application/pdf", "filename": "granatz pótdíj.pdf",
                          "body": { "attachmentId": "A1", "size": 20480 } }
                    ]
                }
            }]
        }))
        .unwrap();
        let mapped = map_thread(&thread);
        let body = &mapped.messages[0].body_text;
        assert!(body.starts_with("Hello"), "{body}");
        assert!(
            body.ends_with("[attachment: granatz pótdíj.pdf (application/pdf, 20480 bytes) #A1]"),
            "{body}"
        );
    }

    #[test]
    fn only_the_primary_inbox_is_loaded() {
        assert!(is_primary_inbox(&thread_with(&[
            "INBOX",
            "CATEGORY_PERSONAL"
        ])));
        assert!(is_primary_inbox(&thread_with(&["INBOX"])));
        assert!(!is_primary_inbox(&thread_with(&[
            "INBOX",
            "CATEGORY_PROMOTIONS"
        ])));
        assert!(!is_primary_inbox(&thread_with(&[
            "INBOX",
            "CATEGORY_UPDATES"
        ])));
        assert!(!is_primary_inbox(&thread_with(&["SENT"])));
        assert!(!is_primary_inbox(&thread_with(&["SPAM"])));
        assert!(!is_primary_inbox(&thread_with(&[])));
    }

    fn fixture(name: &str) -> ApiThread {
        let text = std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn three_message_thread_maps_with_deduplicated_participants() {
        let mapped = map_thread(&fixture("thread_three_messages.json"));
        assert_eq!(mapped.thread.external_id, "t100");
        assert_eq!(mapped.thread.source, Source::Gmail);
        assert_eq!(mapped.thread.subject, "Lunch on Friday?");
        assert_eq!(mapped.messages.len(), 3);
        assert_eq!(
            mapped.thread.participants,
            vec![
                "ana@example.org",
                "me@example.com",
                "bela@example.org",
                "cc@example.org"
            ]
        );
        assert_eq!(
            mapped.messages[0].body_text,
            "Shall we have lunch on Friday?"
        );
        assert_eq!(
            mapped.messages[0].sender_identifier.as_deref(),
            Some("ana@example.org")
        );
        assert!(mapped.messages.iter().all(|m| m.thread_id == "t100"));
        assert_eq!(mapped.thread.last_activity_at, 1_700_000_200);
        assert_eq!(
            mapped.labels,
            Labels {
                inbox: true,
                unread: true,
                sent: true
            }
        );
    }

    fn html_thread(html: &str) -> ApiThread {
        let data = URL_SAFE_NO_PAD.encode(html);
        serde_json::from_value(serde_json::json!({
            "id": "t",
            "messages": [{
                "id": "m",
                "snippet": "Köszönjük regisztrációját!",
                "payload": {
                    "mimeType": "multipart/alternative",
                    "parts": [{ "mimeType": "text/html", "body": { "data": data } }]
                }
            }]
        }))
        .unwrap()
    }

    #[test]
    fn html_only_message_is_reduced_to_text() {
        let html = "<html><head><style>p{color:red}</style></head><body>\
            <p>Köszönjük <b>regisztrációját</b>!</p><script>alert(1)</script>\
            <a href=\"https://x.org/a\">Aktiválás</a></body></html>";
        let mapped = map_thread(&html_thread(html));
        let body = &mapped.messages[0].body_text;
        assert!(!body.starts_with(BODY_UNAVAILABLE_MARKER), "{body}");
        assert!(body.contains("Köszönjük regisztrációját!"), "{body}");
        assert!(body.contains("Aktiválás"), "{body}");
        assert!(!body.contains('<') && !body.contains("alert") && !body.contains("color"));
    }

    #[test]
    fn a_message_with_no_readable_part_keeps_the_snippet_and_is_marked() {
        let mapped = map_thread(&fixture("thread_html_only.json"));
        let body = &mapped.messages[0].body_text;
        assert!(body.contains("Your invoice is ready"), "{body}");
        let empty: ApiThread = serde_json::from_value(serde_json::json!({
            "id": "t",
            "messages": [{ "id": "m", "snippet": "Hi there",
                "payload": { "mimeType": "text/html", "body": {} } }]
        }))
        .unwrap();
        let body = &map_thread(&empty).messages[0].body_text;
        assert!(
            body.starts_with(BODY_UNAVAILABLE_MARKER) && body.contains("Hi there"),
            "{body}"
        );
    }

    #[test]
    fn long_bodies_are_truncated_on_a_character_boundary() {
        let text = "é".repeat(MAX_BODY_BYTES); // two bytes per character
        let cut = truncate_utf8(&text, MAX_BODY_BYTES - 1);
        assert!(cut.len() < MAX_BODY_BYTES);
        assert!(text.starts_with(cut));
        assert_eq!(cut.len() % 2, 0);
        assert_eq!(truncate_utf8("short", 100), "short");
    }

    #[test]
    fn address_parsing_handles_names_quotes_and_commas() {
        assert_eq!(
            addresses(r#""Doe, John" <John@X.org>, bare@y.org, Ana <ana@z.org>"#),
            vec!["john@x.org", "bare@y.org", "ana@z.org"]
        );
        assert!(addresses("undisclosed-recipients:;").is_empty());
    }

    #[test]
    fn mapping_only_reads_labels() {
        // `map_thread` is a pure function of its input: there is no client to write labels with.
        let a = map_thread(&fixture("thread_three_messages.json"));
        let b = map_thread(&fixture("thread_three_messages.json"));
        assert_eq!(a, b);
    }

    #[test]
    fn the_tabs_loaded_come_from_the_names_and_default_to_primary() {
        assert_eq!(Tabs::from_names::<&str>(&[]), Tabs::PRIMARY_ONLY);
        assert_eq!(Tabs::from_names(&["nonsense"]), Tabs::PRIMARY_ONLY);
        assert_eq!(Tabs::from_names(&["All"]), Tabs::ALL);
        let t = Tabs::from_names(&["Updates", "promotions"]);
        assert!(t.updates && t.promotions && !t.primary && !t.social && !t.forums);
        assert!(Tabs::PRIMARY_ONLY.is_primary_only() && !t.is_primary_only());
        let dir = std::env::temp_dir().join("haste-no-gmail-tabs");
        assert_eq!(Tabs::load(&dir), Tabs::PRIMARY_ONLY);
    }

    #[test]
    fn a_thread_is_loaded_when_it_is_in_the_inbox_in_a_tab_that_is_wanted() {
        let promo = thread_with(&["INBOX", "CATEGORY_PROMOTIONS"]);
        let updates = thread_with(&["INBOX", "CATEGORY_UPDATES"]);
        let plain = thread_with(&["INBOX", "CATEGORY_PERSONAL"]);
        let archived = thread_with(&["CATEGORY_UPDATES"]);
        let updates_only = Tabs::from_names(&["updates"]);
        assert!(!is_wanted_inbox(&promo, &Tabs::PRIMARY_ONLY));
        assert!(is_wanted_inbox(&plain, &Tabs::PRIMARY_ONLY));
        assert!(is_wanted_inbox(&updates, &updates_only));
        assert!(!is_wanted_inbox(&promo, &updates_only));
        assert!(!is_wanted_inbox(&plain, &updates_only));
        assert!(is_wanted_inbox(&promo, &Tabs::ALL));
        assert!(
            !is_wanted_inbox(&archived, &Tabs::ALL),
            "archived stays out"
        );
    }
}
