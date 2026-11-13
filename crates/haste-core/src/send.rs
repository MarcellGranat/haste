//! Rules for human-sent mail: limits, address and subject validation, the recipient digest the
//! confirmation carries, and how a send failure is classified. Nothing here does I/O.

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::connector::{CallError, ConnectorError};
use crate::model::Source;

/// Recipients per send. One in v1: no Cc, no Bcc, no reply-all.
pub const MAX_RECIPIENTS: usize = 1;
/// Largest body, in bytes, accepted for a send.
pub const MAX_SEND_BODY_BYTES: usize = 32 * 1024;
/// Longest subject, in bytes.
pub const MAX_SUBJECT_BYTES: usize = 255;
/// Sends allowed per window, counted in the store (so a restart cannot reset it).
pub const MAX_SENDS_PER_HOUR: u32 = 10;
/// The window the send count looks back over.
pub const SEND_WINDOW_SECS: i64 = 3600;
/// Time allowed for one network operation of a send.
pub const SEND_TIMEOUT_SECS: u64 = 30;

/// Largest reply text accepted for a chat send, in bytes.
pub const CHAT_MAX_SEND_BYTES: usize = 4 * 1024;

/// Chat sends per hour for each network's account (Beeper holds one account per network here, so
/// this is per source). The networks can suspend an account that sends too much.
pub const CHAT_MAX_SENDS_PER_HOUR: u32 = 10;
/// Chat sends per day, across all chat sources together: a backstop over the per-account hour.
pub const CHAT_MAX_SENDS_PER_DAY: u32 = 20;
/// The day window the chat limit looks back over.
pub const SEND_DAY_WINDOW_SECS: i64 = 86_400;

/// Which family of limits a send falls under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendScope {
    /// Gmail and iCloud.
    Mail,
    /// WhatsApp, Messenger and iMessage (through Beeper).
    Chat,
}

impl SendScope {
    /// The scope a source is sent under, or `None` when it cannot be sent to at all.
    #[must_use]
    pub fn of(source: &Source) -> Option<Self> {
        match source {
            Source::Gmail | Source::Icloud | Source::Email => Some(Self::Mail),
            Source::Whatsapp | Source::Messenger | Source::Imessage => Some(Self::Chat),
            _ => None,
        }
    }

    /// The source names counted together against this scope's limits.
    #[must_use]
    pub fn sources(self) -> &'static [&'static str] {
        match self {
            Self::Mail => &["gmail", "icloud", "email"],
            Self::Chat => &["whatsapp", "messenger", "imessage"],
        }
    }

    #[must_use]
    pub fn max_per_hour(self) -> u32 {
        match self {
            Self::Mail => MAX_SENDS_PER_HOUR,
            Self::Chat => CHAT_MAX_SENDS_PER_HOUR,
        }
    }

    /// Only chats have a daily cap.
    #[must_use]
    pub fn max_per_day(self) -> Option<u32> {
        match self {
            Self::Mail => None,
            Self::Chat => Some(CHAT_MAX_SENDS_PER_DAY),
        }
    }
}

/// What a failed send certainly (or possibly) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    /// Nothing was delivered and nothing can have been: the proposal returns to `approved`.
    NoEffect,
    /// The mail may or may not have been delivered: the proposal stays `executing`.
    Ambiguous,
    Sent,
}

/// Validate one recipient address. Refuses (never strips) anything that could inject a header or
/// a second recipient: CR, LF, NUL, whitespace, commas and semicolons, plus a minimal
/// `local@domain` shape.
///
/// # Errors
/// A fixed reason label.
pub fn validate_address(address: &str) -> Result<(), &'static str> {
    if address.is_empty() || address.len() > 254 {
        return Err("address_length");
    }
    if address
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, ',' | ';' | '<' | '>' | '"'))
    {
        return Err("address_characters");
    }
    let Some((local, domain)) = address.split_once('@') else {
        return Err("address_shape");
    };
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return Err("address_shape");
    }
    Ok(())
}

/// Validate a subject: no CR, LF or NUL (refused, not stripped), within [`MAX_SUBJECT_BYTES`].
///
/// # Errors
/// A fixed reason label.
pub fn validate_subject(subject: &str) -> Result<(), &'static str> {
    if subject.contains(['\r', '\n', '\0']) {
        return Err("subject_characters");
    }
    if subject.len() > MAX_SUBJECT_BYTES {
        return Err("subject_length");
    }
    Ok(())
}

/// Validate a recipient list: at most [`MAX_RECIPIENTS`], at least one, each address valid.
///
/// # Errors
/// A fixed reason label.
pub fn validate_recipients(recipients: &[String]) -> Result<(), &'static str> {
    if recipients.is_empty() {
        return Err("no_recipient");
    }
    if recipients.len() > MAX_RECIPIENTS {
        return Err("too_many_recipients");
    }
    recipients.iter().try_for_each(|r| validate_address(r))
}

/// A plain-text RFC 5322 message for a human send: `text/plain; charset=UTF-8`, CRLF line ends,
/// no Cc or Bcc header by construction; multipart only when the human attached files. A header value containing CR, LF or NUL is
/// *refused*, never stripped, so what the human confirmed is exactly what goes out.
///
/// # Errors
/// A fixed reason label when a header value contains CR, LF or NUL.
#[allow(clippy::too_many_arguments)]
pub fn compose_plain_text(
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
    date: &str,
    message_id: &str,
    in_reply_to: Option<&str>,
    references: Option<&str>,
    attachments: &[crate::connector::OutAttachment],
) -> Result<String, &'static str> {
    let values = [from, to, subject, date, message_id]
        .into_iter()
        .chain(in_reply_to)
        .chain(references);
    for v in values {
        if v.contains(['\r', '\n', '\0']) {
            return Err("header characters");
        }
    }
    let mut out = String::new();
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str(&format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\n"
    ));
    out.push_str(&format!("Date: {date}\r\nMessage-ID: {message_id}\r\n"));
    if let Some(id) = in_reply_to {
        out.push_str(&format!("In-Reply-To: {id}\r\n"));
    }
    if let Some(refs) = references {
        out.push_str(&format!("References: {refs}\r\n"));
    }
    let text = body.replace("\r\n", "\n").replace('\n', "\r\n");
    if attachments.is_empty() {
        out.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
        out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        out.push_str(&text);
        return Ok(out);
    }
    for a in attachments {
        if a.name.is_empty() || a.name.contains(['\r', '\n', '\0', '"', '\\', '/']) {
            return Err("attachment name");
        }
    }
    let mut boundary = format!("=_haste_{}", ulid::Ulid::new());
    while text.contains(&boundary) {
        boundary = format!("=_haste_{}", ulid::Ulid::new());
    }
    out.push_str(&format!(
        "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"
    ));
    out.push_str(&format!("--{boundary}\r\n"));
    out.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
    out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    out.push_str(&text);
    out.push_str("\r\n");
    for a in attachments {
        let ascii: String = a
            .name
            .chars()
            .map(|c| {
                if c.is_ascii() && !c.is_control() {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        out.push_str(&format!("--{boundary}\r\n"));
        out.push_str(&format!(
            "Content-Type: {}; name=\"{ascii}\"\r\n",
            attachment_mime(&a.name, &a.mime)
        ));
        out.push_str(&format!(
            "Content-Disposition: attachment; filename=\"{ascii}\"; filename*=UTF-8''{}\r\n",
            percent_encode(&a.name)
        ));
        out.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
        let encoded = base64::engine::general_purpose::STANDARD.encode(&a.data);
        for chunk in encoded.as_bytes().chunks(76) {
            out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
            out.push_str("\r\n");
        }
    }
    out.push_str(&format!("--{boundary}--\r\n"));
    Ok(out)
}

/// The MIME type to send: the one given, else a guess from the file extension.
#[must_use]
pub fn attachment_mime(name: &str, given: &str) -> String {
    if !given.is_empty() && !given.contains(['\r', '\n', ';', '"']) {
        return given.to_owned();
    }
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "txt" | "md" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
    .to_owned()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The digest a chat send's confirmation carries: SHA-256 (hex) over the chat id, the chat name
/// the human was shown, and the SHA-256 of the text, each separated by a zero byte. If the chat
/// is renamed or the text changes after the confirmation, the digest no longer matches and the
/// send is refused.
#[must_use]
pub fn chat_send_digest(chat_id: &str, chat_name: &str, body: &str) -> String {
    let body_hash = Sha256::digest(body.as_bytes());
    let mut hasher = Sha256::new();
    hasher.update(chat_id.as_bytes());
    hasher.update([0_u8]);
    hasher.update(chat_name.as_bytes());
    hasher.update([0_u8]);
    hasher.update(body_hash);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `Re: <subject>` without stacking prefixes; an empty subject becomes `(no subject)`.
#[must_use]
pub fn reply_subject(subject: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        return "(no subject)".to_owned();
    }
    if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("re:") {
        return trimmed.to_owned();
    }
    format!("Re: {trimmed}")
}

/// `Mon, 09 Nov 2026 10:00:00 +0000` for unix seconds (always UTC).
#[must_use]
pub fn format_rfc2822(ts: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let weekday = usize::try_from(days.rem_euclid(7)).unwrap_or(0);
    format!(
        "{}, {day:02} {} {year:04} {:02}:{:02}:{:02} +0000",
        DAYS[weekday],
        MONTHS[usize::try_from(month - 1).unwrap_or(0)],
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// `<unique@domain>`: a Message-ID haste generates itself.
#[must_use]
pub fn new_message_id(domain: &str) -> String {
    format!("<{}@{domain}>", crate::auth::new_id().to_lowercase())
}

/// SHA-256 (hex) over the sorted, lowercased recipient list. The confirmation carries the digest
/// of what was displayed; the daemon refuses the send if its own resolution differs.
#[must_use]
pub fn recipients_digest(recipients: &[String]) -> String {
    let mut sorted: Vec<String> = recipients.iter().map(|r| r.to_lowercase()).collect();
    sorted.sort();
    let mut hasher = Sha256::new();
    for r in &sorted {
        hasher.update(r.as_bytes());
        hasher.update([0_u8]);
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Classify a failed send. One attempt is made at every layer, so this only decides where the
/// proposal goes next: back to `approved` when nothing can have been delivered, or left
/// `executing` for a human check when the mail may have gone out.
#[must_use]
pub fn classify(error: &CallError) -> SendOutcome {
    match error {
        CallError::CapabilityMissing(_)
        | CallError::KillSwitchEngaged
        | CallError::BreakerOpened(_) => SendOutcome::NoEffect,
        // the guard makes one call, so exhausted transient retries cannot happen for a send;
        // if they ever did, the safe reading is "may have been delivered"
        CallError::TransientExhausted(_) => SendOutcome::Ambiguous,
        CallError::Connector(e) => match e {
            ConnectorError::Ambiguous(_) => SendOutcome::Ambiguous,
            // refused before anything was sent, a failed connection or handshake, a rejected
            // login, an unsupported call
            ConnectorError::Transient(_)
            | ConnectorError::Other(_)
            | ConnectorError::Unsupported(_)
            | ConnectorError::Authorization(_)
            | ConnectorError::Checkpoint(_)
            | ConnectorError::Challenge(_) => SendOutcome::NoEffect,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_with_anything_that_can_inject_are_refused() {
        for ok in ["a@b.co", "first.last+tag@sub.example.org", "x@y.hu"] {
            assert_eq!(validate_address(ok), Ok(()), "{ok}");
        }
        for (bad, why) in [
            ("", "address_length"),
            ("a@b.co\r\nBcc: evil@x.org", "address_characters"),
            ("a@b.co\nX: y", "address_characters"),
            ("a@b.co\0", "address_characters"),
            ("a@b.co, c@d.co", "address_characters"),
            ("a@b.co;c@d.co", "address_characters"),
            ("a b@c.co", "address_characters"),
            ("<a@b.co>", "address_characters"),
            ("plain", "address_shape"),
            ("@b.co", "address_shape"),
            ("a@", "address_shape"),
            ("a@b", "address_shape"),
            ("a@@b.co", "address_shape"),
            ("a@.co", "address_shape"),
            ("a@b.", "address_shape"),
        ] {
            assert_eq!(validate_address(bad), Err(why), "{bad:?}");
        }
        assert_eq!(
            validate_address(&format!("{}@b.co", "x".repeat(260))),
            Err("address_length")
        );
    }

    #[test]
    fn the_chat_digest_changes_with_the_chat_its_name_or_the_text() {
        let base = chat_send_digest("chat-1", "Family", "see you at six");
        assert_eq!(base.len(), 64);
        assert_eq!(base, chat_send_digest("chat-1", "Family", "see you at six"));
        assert_ne!(base, chat_send_digest("chat-2", "Family", "see you at six"));
        assert_ne!(
            base,
            chat_send_digest("chat-1", "Family (old)", "see you at six")
        );
        assert_ne!(
            base,
            chat_send_digest("chat-1", "Family", "see you at seven")
        );
        // field boundaries matter
        assert_ne!(
            chat_send_digest("ab", "c", "x"),
            chat_send_digest("a", "bc", "x")
        );
    }

    #[test]
    fn chat_sends_have_their_own_stricter_limits_and_mail_has_no_daily_cap() {
        use crate::model::Source::{Gmail, Icloud, Imessage, Messenger, Reminders, Whatsapp};
        assert_eq!(SendScope::of(&Gmail), Some(SendScope::Mail));
        assert_eq!(SendScope::of(&Icloud), Some(SendScope::Mail));
        for s in [Whatsapp, Messenger, Imessage] {
            assert_eq!(SendScope::of(&s), Some(SendScope::Chat));
        }
        assert_eq!(
            SendScope::of(&Reminders),
            None,
            "reminders are not sendable"
        );
        assert_eq!(SendScope::Chat.max_per_hour(), CHAT_MAX_SENDS_PER_HOUR);
        assert_eq!(SendScope::Chat.max_per_day(), Some(CHAT_MAX_SENDS_PER_DAY));
        assert_eq!(SendScope::Mail.max_per_day(), None);
        assert!(SendScope::Chat
            .sources()
            .iter()
            .all(|n| !SendScope::Mail.sources().contains(n)));
    }

    #[test]
    fn the_date_header_is_rfc_2822_in_utc() {
        assert_eq!(format_rfc2822(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(
            format_rfc2822(1_794_218_400),
            "Mon, 09 Nov 2026 10:00:00 +0000"
        );
        assert_eq!(
            format_rfc2822(951_782_400),
            "Tue, 29 Feb 2000 00:00:00 +0000"
        );
        let id = new_message_id("example.org");
        assert!(id.starts_with('<') && id.ends_with("@example.org>"));
        assert_ne!(id, new_message_id("example.org"));
    }

    #[test]
    fn attachments_make_a_multipart_message_with_safe_names() {
        use crate::connector::OutAttachment;
        let d = "Mon, 09 Nov 2026 10:00:00 +0000";
        let file = |name: &str| OutAttachment {
            name: name.into(),
            mime: String::new(),
            data: (0..200).map(|i| u8::try_from(i % 251).unwrap()).collect(),
        };
        let m = compose_plain_text(
            "me@x.org",
            "ana@x.org",
            "Re: Lunch",
            "Hi\nthere",
            d,
            "<i@x>",
            None,
            None,
            &[file("Q3 jelentés.pdf")],
        )
        .unwrap();
        assert!(m.contains("Content-Type: multipart/mixed; boundary=\"=_haste_"));
        assert!(m.contains("Content-Type: application/pdf; name=\"Q3 jelent_s.pdf\""));
        assert!(m.contains("filename*=UTF-8''Q3%20jelent%C3%A9s.pdf"));
        assert!(m.contains("Content-Transfer-Encoding: base64"));
        assert!(m.lines().all(|l| l.len() <= 998));
        assert!(m.trim_end().ends_with("--"));
        assert!(!m.to_lowercase().contains("\r\ncc:") && !m.to_lowercase().contains("\r\nbcc:"));
        for bad in ["a\"b.pdf", "a\r\nBcc: x@y.z", "../x", ""] {
            assert!(
                compose_plain_text(
                    "me@x.org",
                    "ana@x.org",
                    "s",
                    "b",
                    d,
                    "<i@x>",
                    None,
                    None,
                    &[file(bad)]
                )
                .is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_composed_message_refuses_injection_and_has_no_cc_or_bcc() {
        let d = "Mon, 09 Nov 2026 10:00:00 +0000";
        let ok = compose_plain_text(
            "me@x.org",
            "ana@x.org",
            "Re: Lunch",
            "Yes!\nSee you.",
            d,
            "<i@x>",
            Some("<m3@x>"),
            Some("<m1@x> <m3@x>"),
            &[],
        )
        .unwrap();
        let headers = ok.split("\r\n\r\n").next().unwrap();
        for h in [
            "From: me@x.org",
            "To: ana@x.org",
            "Subject: Re: Lunch",
            "Message-ID: <i@x>",
            "In-Reply-To: <m3@x>",
            "References: <m1@x> <m3@x>",
        ] {
            assert!(headers.contains(h), "{h}");
        }
        assert!(!headers.to_lowercase().contains("bcc") && !headers.to_lowercase().contains("cc:"));
        assert!(ok.ends_with("Yes!\r\nSee you."));
        for (to, subject) in [
            ("a@x.org\r\nBcc: e@x.org", "s"),
            ("a@x.org", "s\nBcc: e@x.org"),
            ("a@x.org\0", "s"),
        ] {
            assert_eq!(
                compose_plain_text("me@x.org", to, subject, "b", d, "<i@x>", None, None, &[]),
                Err("header characters"),
                "{to:?} {subject:?}"
            );
        }
    }

    #[test]
    fn reply_subjects_do_not_stack() {
        assert_eq!(reply_subject("Plan"), "Re: Plan");
        assert_eq!(reply_subject("Re: Plan"), "Re: Plan");
        assert_eq!(reply_subject("RE: Plan"), "RE: Plan");
        assert_eq!(reply_subject("  "), "(no subject)");
        assert_eq!(reply_subject("Research"), "Re: Research");
    }

    #[test]
    fn subjects_and_recipient_lists_are_validated() {
        assert_eq!(validate_subject("Re: plan"), Ok(()));
        for bad in ["a\r\nBcc: x", "a\nb", "a\0b"] {
            assert_eq!(validate_subject(bad), Err("subject_characters"));
        }
        assert_eq!(
            validate_subject(&"s".repeat(MAX_SUBJECT_BYTES + 1)),
            Err("subject_length")
        );
        assert_eq!(validate_subject(&"s".repeat(MAX_SUBJECT_BYTES)), Ok(()));
        let one = vec!["a@b.co".to_owned()];
        assert_eq!(validate_recipients(&one), Ok(()));
        assert_eq!(validate_recipients(&[]), Err("no_recipient"));
        let two = vec!["a@b.co".to_owned(), "c@d.co".to_owned()];
        assert_eq!(validate_recipients(&two), Err("too_many_recipients"));
        assert_eq!(
            validate_recipients(&["bad".to_owned()]),
            Err("address_shape")
        );
    }

    #[test]
    fn the_digest_ignores_order_and_case_but_any_change_alters_it() {
        let a = vec!["a@b.co".to_owned(), "c@d.co".to_owned()];
        let b = vec!["C@D.co".to_owned(), "a@b.co".to_owned()];
        assert_eq!(recipients_digest(&a), recipients_digest(&b));
        assert_ne!(
            recipients_digest(&a),
            recipients_digest(&["a@b.co".to_owned()])
        );
        assert_ne!(
            recipients_digest(&["a@b.co".to_owned()]),
            recipients_digest(&["a@b.com".to_owned()])
        );
        assert_eq!(recipients_digest(&a).len(), 64);
        // boundaries matter: two addresses are not one longer address
        assert_ne!(
            recipients_digest(&["ab@c.co".to_owned()]),
            recipients_digest(&["a@c.co".to_owned(), "b@c.co".to_owned()])
        );
    }

    #[test]
    fn failures_classify_by_whether_anything_can_have_been_delivered() {
        use ConnectorError::{Ambiguous, Authorization, Checkpoint, Other, Transient};
        for (e, expect) in [
            (CallError::KillSwitchEngaged, SendOutcome::NoEffect),
            (CallError::CapabilityMissing("send"), SendOutcome::NoEffect),
            (
                CallError::BreakerOpened(Checkpoint("x".into())),
                SendOutcome::NoEffect,
            ),
            (
                CallError::Connector(Other("send refused".into())),
                SendOutcome::NoEffect,
            ),
            (
                CallError::Connector(Transient("connect".into())),
                SendOutcome::NoEffect,
            ),
            (
                CallError::Connector(Authorization("x".into())),
                SendOutcome::NoEffect,
            ),
            (
                CallError::Connector(Ambiguous("after data".into())),
                SendOutcome::Ambiguous,
            ),
            (
                CallError::TransientExhausted(Transient("x".into())),
                SendOutcome::Ambiguous,
            ),
        ] {
            assert_eq!(classify(&e), expect, "{e:?}");
        }
    }
}
