//! Plain-text MIME drafts. Header values are stripped of line breaks (no header injection).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

fn clean(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_owned()
}

/// `Re: ` prefix unless the subject already has one.
#[must_use]
pub fn reply_subject(subject: &str) -> String {
    let subject = clean(subject);
    if subject.to_lowercase().starts_with("re:") {
        subject
    } else {
        format!("Re: {subject}")
    }
}

/// A single-part `text/plain; charset=UTF-8` message. No HTML, no multipart.
#[must_use]
pub fn build_plain_text(
    to: &str,
    subject: &str,
    body: &str,
    in_reply_to: Option<&str>,
    references: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str(&format!("To: {}\r\n", clean(to)));
    out.push_str(&format!("Subject: {}\r\n", clean(subject)));
    if let Some(id) = in_reply_to {
        out.push_str(&format!("In-Reply-To: {}\r\n", clean(id)));
    }
    if let Some(refs) = references {
        out.push_str(&format!("References: {}\r\n", clean(refs)));
    }
    out.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
    out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    out.push_str(&body.replace("\r\n", "\n").replace('\n', "\r\n"));
    out
}

/// A plain-text message for a human send (see [`haste_core::send::compose_plain_text`]): header
/// values with a line break or NUL are refused, not stripped.
///
/// # Errors
/// A fixed reason label when a header value contains CR, LF or NUL.
#[allow(clippy::too_many_arguments)]
pub fn build_send_message(
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
    date: &str,
    message_id: &str,
    in_reply_to: Option<&str>,
    references: Option<&str>,
) -> Result<String, &'static str> {
    haste_core::send::compose_plain_text(
        from,
        to,
        subject,
        body,
        date,
        message_id,
        in_reply_to,
        references,
    )
}

/// Gmail's `raw` field: base64url without padding.
#[must_use]
pub fn encode_raw(mime: &str) -> String {
    URL_SAFE_NO_PAD.encode(mime.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_is_single_part_plain_text_with_threading_headers() {
        let mime = build_plain_text(
            "ana@x.org",
            "Re: Lunch",
            "Yes!\nSee you.",
            Some("<m3@x>"),
            Some("<m1@x> <m3@x>"),
        );
        assert!(mime.contains("Content-Type: text/plain; charset=UTF-8\r\n"));
        assert!(mime.contains("In-Reply-To: <m3@x>\r\n"));
        assert!(mime.contains("References: <m1@x> <m3@x>\r\n"));
        assert!(mime.ends_with("Yes!\r\nSee you."));
        let lower = mime.to_lowercase();
        assert!(!lower.contains("text/html") && !lower.contains("multipart"));
        assert_eq!(lower.matches("content-type:").count(), 1);
    }

    #[test]
    fn header_injection_is_neutralised() {
        let mime = build_plain_text(
            "a@x.org\r\nBcc: evil@x.org",
            "Hi\nBcc: evil@x.org",
            "b",
            None,
            None,
        );
        let headers = mime.split("\r\n\r\n").next().unwrap();
        assert!(!headers.lines().any(|l| l.starts_with("Bcc:")), "{headers}");
    }

    #[test]
    fn a_send_message_refuses_injection_and_has_no_cc_or_bcc() {
        let ok = build_send_message(
            "me@x.org",
            "ana@x.org",
            "Re: Lunch",
            "Yes!\nSee you.",
            "Mon, 09 Nov 2026 10:00:00 +0000",
            "<id@x.org>",
            Some("<m3@x>"),
            Some("<m1@x> <m3@x>"),
        )
        .unwrap();
        let headers = ok.split("\r\n\r\n").next().unwrap();
        for h in [
            "From: me@x.org",
            "To: ana@x.org",
            "Subject: Re: Lunch",
            "Message-ID: <id@x.org>",
            "In-Reply-To: <m3@x>",
        ] {
            assert!(headers.contains(h), "{h}");
        }
        assert!(!headers.to_lowercase().contains("bcc") && !headers.to_lowercase().contains("cc:"));
        assert!(ok.ends_with("Yes!\r\nSee you."));
        let d = "Mon, 09 Nov 2026 10:00:00 +0000";
        for (to, subject) in [
            ("a@x.org\r\nBcc: e@x.org", "s"),
            ("a@x.org", "s\nBcc: e@x.org"),
            ("a@x.org\0", "s"),
        ] {
            assert_eq!(
                build_send_message("me@x.org", to, subject, "b", d, "<i@x>", None, None),
                Err("header characters"),
                "{to:?} {subject:?}"
            );
        }
    }

    #[test]
    fn subject_gets_one_re_prefix() {
        assert_eq!(reply_subject("Lunch"), "Re: Lunch");
        assert_eq!(reply_subject("Re: Lunch"), "Re: Lunch");
    }

    #[test]
    fn raw_is_urlsafe_without_padding() {
        let raw = encode_raw("any bytes ??>>");
        assert!(!raw.contains('=') && !raw.contains('+') && !raw.contains('/'));
    }
}
