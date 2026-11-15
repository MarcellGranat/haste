//! Reactions on a chat message, stored as one text line of the message body so the schema stays
//! as it is: `[reactions: 👍×2 ❤️]`. Keys are the emoji (or the network's short name); a key
//! used by several people carries `×count`.

const PREFIX: &str = "[reactions: ";
/// Longest reaction key kept (a custom emoji name can be long).
const MAX_KEY_BYTES: usize = 32;
/// Most distinct reactions shown for one message.
const MAX_KINDS: usize = 12;

/// The body line for these reaction keys (one per person who reacted), or `None` when there are
/// none. Keys with control characters, brackets or no text are dropped.
#[must_use]
pub fn format_line<'a>(keys: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for key in keys {
        let key = key.trim();
        if key.is_empty()
            || key.len() > MAX_KEY_BYTES
            || key
                .chars()
                .any(|c| c.is_control() || matches!(c, '[' | ']' | '×'))
        {
            continue;
        }
        match counts.iter_mut().find(|(k, _)| k == key) {
            Some((_, n)) => *n += 1,
            None => counts.push((key.to_owned(), 1)),
        }
    }
    counts.truncate(MAX_KINDS);
    if counts.is_empty() {
        return None;
    }
    let shown: Vec<String> = counts
        .into_iter()
        .map(|(k, n)| if n > 1 { format!("{k}×{n}") } else { k })
        .collect();
    Some(format!("{PREFIX}{}]", shown.join(" ")))
}

/// Split a body into its text and the reactions it carries (the part between the brackets).
#[must_use]
pub fn take(body: &str) -> (String, Option<String>) {
    let mut text = Vec::new();
    let mut reactions = None;
    for line in body.lines() {
        match line.strip_prefix(PREFIX).and_then(|r| r.strip_suffix(']')) {
            Some(r) if reactions.is_none() => reactions = Some(r.to_owned()),
            _ => text.push(line),
        }
    }
    (text.join("\n"), reactions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactions_are_counted_and_round_trip() {
        let line = format_line(["👍", "❤️", "👍", " ", "bad]key"]).unwrap();
        assert_eq!(line, "[reactions: 👍×2 ❤️]");
        let (text, reactions) = take(&format!("hello\n{line}"));
        assert_eq!(
            (text.as_str(), reactions.as_deref()),
            ("hello", Some("👍×2 ❤️"))
        );
        assert!(format_line(Vec::<&str>::new()).is_none());
        assert_eq!(take("plain"), ("plain".to_owned(), None));
    }
}
