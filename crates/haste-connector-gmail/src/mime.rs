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
