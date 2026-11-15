//! RFC 5322 headers and text bodies into the haste data model, plus thread grouping.
//! `text/plain` is preferred; `text/html` is read only when there is no plain part and is reduced
//! to text, never rendered. Attachment bodies are never fetched.

use std::collections::HashMap;

use haste_core::connector::Batch;
use haste_core::model::{Message, Source, Thread};
use mail_parser::MessageParser;

use crate::session::{AttachmentMeta, HeaderFetch};

/// Stored plain-text body limit, in bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Stored in place of a body when a message has neither a `text/plain` nor a `text/html` part.
pub const BODY_UNAVAILABLE_MARKER: &str = "[body unavailable]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMessage {
    pub folder: String,
    pub uid_validity: u32,
    pub uid: u32,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Option<String>,
    pub participants: Vec<String>,
    pub date: i64,
    pub body_text: String,
    pub attachments: Vec<AttachmentMeta>,
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

/// Lowercase subject without `Re:`/`Fwd:`/`Fw:` prefixes and extra spaces.
#[must_use]
pub fn normalise_subject(subject: &str) -> String {
    let mut s = subject.trim();
    loop {
        let lower = s.to_lowercase();
        let stripped = ["re:", "fwd:", "fw:"]
            .iter()
            .find_map(|p| lower.starts_with(p).then_some(p.len()));
        match stripped {
            Some(n) => s = s[n..].trim_start(),
            None => break,
        }
    }
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_reply_subject(subject: &str) -> bool {
    let lower = subject.trim().to_lowercase();
    ["re:", "fwd:", "fw:"].iter().any(|p| lower.starts_with(p))
}

fn safe_token(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

/// Decode a fetched text part by wrapping it in a minimal MIME entity, so charset and
/// transfer-encoding handling comes from the parser. For `text/html` the decoded markup is then
/// reduced to text (style/script/comment blocks dropped, tags removed, entities decoded).
/// Nothing is rendered and no address inside the HTML is ever contacted.
fn decode_text(body: &[u8], charset: &str, encoding: &str, html: bool) -> Option<String> {
    let mut raw = format!(
        "Content-Type: text/plain; charset=\"{}\"\r\nContent-Transfer-Encoding: {}\r\n\r\n",
        safe_token(charset),
        safe_token(encoding)
    )
    .into_bytes();
    raw.extend_from_slice(body);
    let message = MessageParser::default().parse(&raw)?;
    let decoded = message.body_text(0)?.into_owned();
    if html {
        Some(haste_core::html::to_text(&decoded))
    } else {
        Some(decoded)
    }
}

/// Parse one fetched message. `body` is the already-fetched text part (`fetch.text`), if any.
#[must_use]
pub fn parse_message(
    folder: &str,
    uid_validity: u32,
    fetch: &HeaderFetch,
    body: Option<&[u8]>,
) -> Option<ParsedMessage> {
    let message = MessageParser::default().parse(&fetch.header)?;
    let list = |a: Option<&mail_parser::Address<'_>>| -> Vec<String> {
        a.and_then(|a| a.as_list())
            .map(|l| {
                l.iter()
                    .filter_map(|x| x.address.as_deref())
                    .map(str::to_lowercase)
                    .collect()
            })
            .unwrap_or_default()
    };
    let from = list(message.from());
    let mut participants = from.clone();
    for a in list(message.to()).into_iter().chain(list(message.cc())) {
        if !participants.contains(&a) {
            participants.push(a);
        }
    }
    let body_text = match (&fetch.text, body) {
        (Some(part), Some(bytes)) => decode_text(bytes, &part.charset, &part.encoding, part.html)
            .map(|t| truncate_utf8(&t, MAX_BODY_BYTES).to_owned())
            .unwrap_or_else(|| BODY_UNAVAILABLE_MARKER.to_owned()),
        _ => BODY_UNAVAILABLE_MARKER.to_owned(),
    };
    Some(ParsedMessage {
        folder: folder.to_owned(),
        uid_validity,
        uid: fetch.uid,
        message_id: message.message_id().map(str::to_owned),
        in_reply_to: message.in_reply_to().as_text().map(str::to_owned),
        references: message
            .references()
            .as_text_list()
            .map(|l| l.into_iter().map(str::to_owned).collect())
            .unwrap_or_default(),
        subject: message.subject().unwrap_or("").to_owned(),
        from: from.into_iter().next(),
        participants,
        date: message
            .date()
            .map_or(0, mail_parser::DateTime::to_timestamp),
        body_text,
        attachments: fetch.attachments.clone(),
    })
}

/// Thread key: References root, else In-Reply-To (resolved inside the batch), else a header-less
/// reply joins its subject's thread, else the message's own id.
fn thread_key(
    m: &ParsedMessage,
    by_id: &HashMap<String, String>,
    by_subject: &HashMap<(String, String), String>,
) -> String {
    if let Some(root) = m.references.first() {
        return root.clone();
    }
    if let Some(parent) = &m.in_reply_to {
        return by_id.get(parent).cloned().unwrap_or_else(|| parent.clone());
    }
    let norm = normalise_subject(&m.subject);
    if is_reply_subject(&m.subject) {
        if let Some(key) = by_subject.get(&(m.folder.clone(), norm.clone())) {
            return key.clone();
        }
        return format!("subject:{}:{norm}", m.folder);
    }
    m.message_id
        .clone()
        .unwrap_or_else(|| format!("subject:{}:{norm}", m.folder))
}

/// Group parsed messages into threads. Keys come from headers only, so re-syncing the same
/// messages always yields the same ids. `Message::thread_id` carries the thread's external id.
#[must_use]
pub fn build_batch(mut parsed: Vec<ParsedMessage>) -> Batch {
    parsed.sort_by_key(|m| (m.date, m.uid));
    let mut by_id: HashMap<String, String> = HashMap::new();
    let mut by_subject: HashMap<(String, String), String> = HashMap::new();
    let mut keys = Vec::new();
    for m in &parsed {
        let key = thread_key(m, &by_id, &by_subject);
        if let Some(id) = &m.message_id {
            by_id.insert(id.clone(), key.clone());
        }
        if m.references.is_empty() && m.in_reply_to.is_none() && !is_reply_subject(&m.subject) {
            by_subject.insert(
                (m.folder.clone(), normalise_subject(&m.subject)),
                key.clone(),
            );
        }
        keys.push(key);
    }

    let mut threads: Vec<Thread> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut messages = Vec::new();
    for (m, key) in parsed.iter().zip(keys) {
        let at = *index.entry(key.clone()).or_insert_with(|| {
            threads.push(Thread {
                id: key.clone(),
                source: Source::Icloud,
                external_id: key.clone(),
                subject: String::new(),
                participants: Vec::new(),
                last_activity_at: 0,
                version: 0,
            });
            threads.len() - 1
        });
        let t = &mut threads[at];
        if t.subject.is_empty() {
            t.subject = if m.subject.trim().is_empty() {
                "(no subject)".to_owned()
            } else {
                m.subject.trim().to_owned()
            };
            // A thread is named after its original subject, not after "Re: ...".
            if is_reply_subject(&t.subject) {
                t.subject = normalise_subject(&t.subject);
            }
        }
        for p in &m.participants {
            if !t.participants.contains(p) {
                t.participants.push(p.clone());
            }
        }
        t.last_activity_at = t.last_activity_at.max(m.date);

        let mut body = m.body_text.clone();
        for a in &m.attachments {
            body.push('\n');
            body.push_str(&haste_core::attachment::format_line(
                a.name.as_deref().unwrap_or("unnamed"),
                &a.content_type,
                u64::from(a.size),
                Some(&format!("{}|{}", a.section, a.encoding)),
            ));
        }
        messages.push(Message {
            id: String::new(),
            thread_id: key_of(&threads, at),
            source: Source::Icloud,
            external_id: m
                .message_id
                .clone()
                .unwrap_or_else(|| format!("{}:{}:{}", m.folder, m.uid_validity, m.uid)),
            sender_person_id: None,
            sender_identifier: m.from.clone(),
            body_text: body,
            sent_at: m.date,
            is_own: false,
        });
    }
    Batch {
        threads,
        messages,
        next_cursor: None,
        ..Batch::default()
    }
}

fn key_of(threads: &[Thread], at: usize) -> String {
    threads[at].external_id.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TextPart;

    fn header(extra: &str) -> Vec<u8> {
        format!("From: Ana <Ana@Example.org>\r\nTo: me@icloud.com, Bela <bela@example.org>\r\nDate: Tue, 14 Nov 2023 22:13:20 +0000\r\n{extra}\r\n\r\n").into_bytes()
    }

    fn fetch(uid: u32, extra: &str, plain: bool) -> HeaderFetch {
        HeaderFetch {
            uid,
            header: header(extra),
            text: plain.then(|| TextPart {
                html: false,
                section: vec![1],
                charset: "utf-8".into(),
                encoding: "7bit".into(),
                octets: 10,
            }),
            attachments: Vec::new(),
        }
    }

    fn parsed(uid: u32, extra: &str, body: Option<&[u8]>) -> ParsedMessage {
        parse_message("INBOX", 42, &fetch(uid, extra, body.is_some()), body).unwrap()
    }

    #[test]
    fn headers_and_plain_body_are_mapped() {
        let m = parsed(
            1,
            "Subject: Lunch?\r\nMessage-ID: <a@x>",
            Some(b"See you at noon."),
        );
        assert_eq!(m.message_id.as_deref(), Some("a@x"));
        assert_eq!(m.subject, "Lunch?");
        assert_eq!(m.from.as_deref(), Some("ana@example.org"));
        assert_eq!(
            m.participants,
            vec!["ana@example.org", "me@icloud.com", "bela@example.org"]
        );
        assert_eq!(m.body_text, "See you at noon.");
        assert_eq!(m.date, 1_700_000_000);
    }

    #[test]
    fn quoted_printable_and_charsets_are_decoded() {
        let mut f = fetch(1, "Subject: x\r\nMessage-ID: <q@x>", true);
        f.text = Some(TextPart {
            html: false,
            section: vec![1],
            charset: "iso-8859-1".into(),
            encoding: "quoted-printable".into(),
            octets: 20,
        });
        let m = parse_message("INBOX", 1, &f, Some(b"caf=E9 =3D ok")).unwrap();
        assert_eq!(m.body_text, "café = ok");
    }

    #[test]
    fn missing_plain_part_is_marked_not_fetched() {
        let m = parsed(1, "Subject: HTML only\r\nMessage-ID: <h@x>", None);
        assert_eq!(m.body_text, BODY_UNAVAILABLE_MARKER);
    }

    fn html_message(body: &[u8], encoding: &str, charset: &str) -> ParsedMessage {
        let mut f = fetch(1, "Subject: News\r\nMessage-ID: <n@x>", true);
        f.text = Some(TextPart {
            html: true,
            section: vec![1],
            charset: charset.into(),
            encoding: encoding.into(),
            octets: u32::try_from(body.len()).unwrap(),
        });
        parse_message("INBOX", 1, &f, Some(body)).unwrap()
    }

    #[test]
    fn html_is_reduced_to_text_without_markup_scripts_styles_or_pixels() {
        let html = b"<html><head><style>p{color:red}</style><script>alert('boom')</script></head>\
            <body><p>Hello <b>Ana</b>,</p><p>Meet &amp; greet</p>\
            <img src=\"http://tracker.example/pixel.gif\"></body></html>";
        let m = html_message(html, "7bit", "utf-8");
        assert!(m.body_text.contains("Hello Ana"), "{:?}", m.body_text);
        assert!(m.body_text.contains("Meet & greet"), "{:?}", m.body_text);
        for leaked in [
            "<",
            ">",
            "alert",
            "boom",
            "color:red",
            "tracker.example",
            "pixel",
        ] {
            assert!(
                !m.body_text.contains(leaked),
                "{leaked:?} left in {:?}",
                m.body_text
            );
        }
    }

    #[test]
    fn style_and_script_blocks_inside_the_body_are_dropped_in_any_case() {
        let html = b"<BODY><STYLE type=\"text/css\">p{color:red}</STYLE><p>Keep <!-- secret comment -->this</p>\
            <Script>var tracker = 1;</Script ><p>and this</p><style>never closed";
        let m = html_message(html, "7bit", "utf-8");
        let words = m.body_text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(words, "Keep this and this", "{:?}", m.body_text);
    }

    #[test]
    fn base64_html_is_decoded_then_reduced() {
        let m = html_message(
            b"PHA+SGVsbG8gYmFzZTY0IHdvcmxkPC9wPjxwPlNlY29uZCBwYXJhZ3JhcGg8L3A+",
            "base64",
            "utf-8",
        );
        assert_eq!(
            m.body_text.split_whitespace().collect::<Vec<_>>().join(" "),
            "Hello base64 world Second paragraph"
        );
    }

    #[test]
    fn a_cut_off_base64_html_part_still_gives_the_text_it_has() {
        let m = html_message(b"PHA+SGVsbG8gYmFzZTY0IHdvcmxkPC", "base64", "utf-8");
        assert!(m.body_text.contains("Hello"), "{:?}", m.body_text);
        assert_ne!(m.body_text, BODY_UNAVAILABLE_MARKER);
    }

    #[test]
    fn html_charsets_and_entities_are_decoded() {
        let m = html_message(
            b"<p>caf=E9 &eacute; &#8364;</p>",
            "quoted-printable",
            "iso-8859-1",
        );
        assert!(m.body_text.contains("café"), "{:?}", m.body_text);
        assert!(m.body_text.contains('€'), "{:?}", m.body_text);
    }

    #[test]
    fn plain_text_wins_when_the_message_points_at_a_plain_part() {
        // `fetch.text` already prefers plain (see session::Found); the decoder honours `html`.
        let m = parsed(1, "Subject: Both\r\nMessage-ID: <b@x>", Some(b"plain wins"));
        assert_eq!(m.body_text, "plain wins");
    }

    #[test]
    fn long_bodies_are_cut_on_a_character_boundary() {
        let text = "é".repeat(MAX_BODY_BYTES);
        let cut = truncate_utf8(&text, MAX_BODY_BYTES - 1);
        assert!(cut.len() < MAX_BODY_BYTES && text.starts_with(cut) && cut.len().is_multiple_of(2));
    }

    #[test]
    fn reply_joins_its_thread_through_references() {
        let root = parsed(1, "Subject: Lunch?\r\nMessage-ID: <a@x>", Some(b"one"));
        let reply = parsed(
            2,
            "Subject: Re: Lunch?\r\nMessage-ID: <b@x>\r\nIn-Reply-To: <a@x>\r\nReferences: <a@x>",
            Some(b"two"),
        );
        let batch = build_batch(vec![root, reply]);
        assert_eq!(batch.threads.len(), 1);
        assert_eq!(batch.threads[0].external_id, "a@x");
        assert_eq!(batch.threads[0].subject, "Lunch?");
        assert!(batch.messages.iter().all(|m| m.thread_id == "a@x"));
    }

    #[test]
    fn in_reply_to_alone_resolves_through_the_batch() {
        let root = parsed(1, "Subject: Plan\r\nMessage-ID: <r@x>", Some(b"a"));
        let mid = parsed(
            2,
            "Subject: Re: Plan\r\nMessage-ID: <m@x>\r\nReferences: <r@x>",
            Some(b"b"),
        );
        let leaf = parsed(
            3,
            "Subject: Re: Plan\r\nMessage-ID: <l@x>\r\nIn-Reply-To: <m@x>",
            Some(b"c"),
        );
        let batch = build_batch(vec![root, mid, leaf]);
        assert_eq!(batch.threads.len(), 1);
    }

    #[test]
    fn headerless_reply_falls_back_to_the_normalised_subject() {
        let root = parsed(1, "Subject: Invoice  42\r\nMessage-ID: <i@x>", Some(b"a"));
        let reply = parsed(
            2,
            "Subject: RE: invoice 42\r\nMessage-ID: <j@x>",
            Some(b"b"),
        );
        let batch = build_batch(vec![root, reply]);
        assert_eq!(batch.threads.len(), 1);
        // With no root in the batch the subject key is still deterministic and folder-scoped.
        let alone = build_batch(vec![parsed(
            5,
            "Subject: Re: Invoice 42\r\nMessage-ID: <k@x>",
            Some(b"c"),
        )]);
        assert_eq!(alone.threads[0].external_id, "subject:INBOX:invoice 42");
    }

    #[test]
    fn unrelated_messages_stay_in_separate_threads() {
        let a = parsed(1, "Subject: A\r\nMessage-ID: <a@x>", Some(b"1"));
        let b = parsed(2, "Subject: B\r\nMessage-ID: <b@x>", Some(b"2"));
        assert_eq!(build_batch(vec![a, b]).threads.len(), 2);
    }

    #[test]
    fn attachment_metadata_is_listed_without_content() {
        let mut f = fetch(1, "Subject: Report\r\nMessage-ID: <r@x>", true);
        f.attachments = vec![AttachmentMeta {
            name: Some("q3.pdf".into()),
            content_type: "application/pdf".into(),
            size: 120_000,
            section: "2".into(),
            encoding: "base64".into(),
        }];
        let m = parse_message("INBOX", 1, &f, Some(b"see attached")).unwrap();
        let batch = build_batch(vec![m]);
        assert!(batch.messages[0]
            .body_text
            .contains("[attachment: q3.pdf (application/pdf, 120000 bytes) #2|base64]"));
    }

    #[test]
    fn rebuilding_the_same_messages_gives_identical_ids() {
        let make = || {
            vec![
                parsed(1, "Subject: A\r\nMessage-ID: <a@x>", Some(b"1")),
                parsed(
                    2,
                    "Subject: Re: A\r\nMessage-ID: <b@x>\r\nReferences: <a@x>",
                    Some(b"2"),
                ),
            ]
        };
        let (one, two) = (build_batch(make()), build_batch(make()));
        assert_eq!(one.threads, two.threads);
        assert_eq!(one.messages, two.messages);
    }

    #[test]
    fn message_without_message_id_gets_a_uid_based_id() {
        let m = parsed(7, "Subject: No id", Some(b"x"));
        let batch = build_batch(vec![m]);
        assert_eq!(batch.messages[0].external_id, "INBOX:42:7");
    }

    #[test]
    fn subject_normalisation() {
        assert_eq!(
            normalise_subject("  RE: Re:  Fwd: Hello   World "),
            "hello world"
        );
    }
}
