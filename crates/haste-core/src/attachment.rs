//! The one-line form an attachment takes inside a stored message body:
//! `[attachment: name (type, N bytes) #ref]`. The `#ref` is optional and opaque: a connector
//! writes whatever it needs to fetch the file again later (Gmail's attachment id, an IMAP part
//! and its encoding, a Beeper asset). Only metadata is stored, never the file.

/// An attachment as one stored line describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentLine {
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    /// What the connector needs to fetch it again (no spaces); `None` for older lines.
    pub reference: Option<String>,
}

/// The line for an attachment. `name` and `mime` are cleaned of control characters and of the
/// characters the format uses; `reference` keeps only its visible, space-free characters.
#[must_use]
pub fn format_line(name: &str, mime: &str, bytes: u64, reference: Option<&str>) -> String {
    let clean = |s: &str, max: usize| -> String {
        s.chars()
            .filter(|c| !c.is_control() && *c != ']')
            .take(max)
            .collect::<String>()
            .trim()
            .to_owned()
    };
    let name = clean(name, 120);
    let name = if name.is_empty() {
        "unnamed".to_owned()
    } else {
        name
    };
    let mime = clean(mime, 60).replace(['(', ')', ','], "");
    let reference: Option<String> = reference
        .map(|r| {
            r.chars()
                .filter(|c| !c.is_control() && !c.is_whitespace() && *c != ']')
                .take(400)
                .collect()
        })
        .filter(|r: &String| !r.is_empty());
    match reference {
        Some(r) => format!("[attachment: {name} ({mime}, {bytes} bytes) #{r}]"),
        None => format!("[attachment: {name} ({mime}, {bytes} bytes)]"),
    }
}

/// Parse one line; `None` when it is not an attachment line. A bare `[attachment]` (older
/// Beeper rows) is read as an unnamed one without a reference.
#[must_use]
pub fn parse_line(line: &str) -> Option<AttachmentLine> {
    let line = line.trim();
    if line == "[attachment]" {
        return Some(AttachmentLine {
            name: "attachment".to_owned(),
            mime: String::new(),
            bytes: 0,
            reference: None,
        });
    }
    let inner = line.strip_prefix("[attachment: ")?.strip_suffix(']')?;
    let (inner, reference) = match inner.rsplit_once(" #") {
        Some((head, r)) if !r.is_empty() && !r.contains(' ') => (head, Some(r.to_owned())),
        _ => (inner, None),
    };
    let (name, rest) = match inner.rsplit_once(" (") {
        Some((n, r)) => (n, r.trim_end_matches(')')),
        None => (inner, ""),
    };
    let mut parts = rest.split(", ");
    let mime = parts.next().unwrap_or("").to_owned();
    let bytes = parts
        .next()
        .and_then(|s| s.strip_suffix(" bytes"))
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
    Some(AttachmentLine {
        name: name.to_owned(),
        mime,
        bytes,
        reference,
    })
}

/// Every attachment line of a message body, in order.
#[must_use]
pub fn lines_of(body: &str) -> Vec<AttachmentLine> {
    body.lines().filter_map(parse_line).collect()
}

/// A file name that is safe to create: no path parts, no control characters, no leading dot, a
/// bounded length. Used for every file the daemon writes from an attachment.
#[must_use]
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_owned();
    let cleaned: String = cleaned.chars().take(120).collect();
    if cleaned.is_empty() {
        "attachment".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_round_trips_with_and_without_a_reference() {
        let line = format_line(
            "granatz pótdíj.pdf",
            "application/pdf",
            20480,
            Some("ANGjd-A_1"),
        );
        assert_eq!(
            line,
            "[attachment: granatz pótdíj.pdf (application/pdf, 20480 bytes) #ANGjd-A_1]"
        );
        let a = parse_line(&line).unwrap();
        assert_eq!(
            (
                a.name.as_str(),
                a.mime.as_str(),
                a.bytes,
                a.reference.as_deref()
            ),
            (
                "granatz pótdíj.pdf",
                "application/pdf",
                20480,
                Some("ANGjd-A_1")
            )
        );
        let old = parse_line("[attachment: q3.pdf (application/pdf, 120000 bytes)]").unwrap();
        assert_eq!((old.bytes, old.reference), (120_000, None));
        assert!(parse_line("an ordinary line").is_none());
        assert_eq!(parse_line("[attachment]").unwrap().name, "attachment");
    }

    #[test]
    fn names_with_brackets_and_spaces_in_references_cannot_break_the_format() {
        let line = format_line(
            "a (1).pdf]\n[attachment: x",
            "text/plain",
            3,
            Some("a b#c]"),
        );
        let parsed = lines_of(&line);
        assert_eq!(parsed.len(), 1, "{line}");
        assert!(!parsed[0].name.contains(']'));
        assert_eq!(parsed[0].reference.as_deref(), Some("ab#c"));
    }

    #[test]
    fn saved_file_names_are_plain() {
        assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_file_name(".hidden"), "hidden");
        assert_eq!(safe_file_name("Q3 report.pdf"), "Q3 report.pdf");
        assert_eq!(safe_file_name("   "), "attachment");
    }
}
