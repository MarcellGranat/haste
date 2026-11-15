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

/// Whether a key is one a reaction may be sent with: short, printable, no brackets.
#[must_use]
pub fn valid_key(key: &str) -> bool {
    let key = key.trim();
    !key.is_empty()
        && key.len() <= MAX_KEY_BYTES
        && !key
            .chars()
            .any(|c| c.is_control() || matches!(c, '[' | ']' | '×'))
}

/// The body with one more person's `key` reaction added to its reactions line.
#[must_use]
pub fn add_key(body: &str, key: &str) -> String {
    let (text, existing) = take(body);
    let mut keys: Vec<String> = Vec::new();
    for item in existing.as_deref().unwrap_or("").split_whitespace() {
        let (k, n) = item
            .rsplit_once('×')
            .map_or((item, 1), |(k, n)| (k, n.parse::<usize>().unwrap_or(1)));
        keys.extend(std::iter::repeat_n(k.to_owned(), n.min(1000)));
    }
    keys.push(key.trim().to_owned());
    match format_line(keys.iter().map(String::as_str)) {
        Some(line) => format!("{text}\n{line}"),
        None => text,
    }
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

    fn line_body() -> String {
        "hi\n[reactions: 👍×2 ❤️]".to_owned()
    }

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
        assert_eq!(add_key("hi", "👍"), "hi\n[reactions: 👍]");
        assert_eq!(add_key(&line_body(), "👍"), "hi\n[reactions: 👍×3 ❤️]");
        assert_eq!(
            add_key("hi\n[reactions: 👍×2 ❤️]", "😂"),
            "hi\n[reactions: 👍×2 ❤️ 😂]"
        );
        assert!(valid_key("👍") && !valid_key("") && !valid_key("a]b") && !valid_key("x\u{7}y"));
    }
}
