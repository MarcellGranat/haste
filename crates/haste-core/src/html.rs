//! HTML to plain text for message bodies. Nothing is rendered and no address inside the markup
//! is ever contacted.

/// Remove `<style>` and `<script>` blocks and `<!-- comments -->` (case-insensitive). The
/// parser's own HTML-to-text keeps `<style>` text that sits inside `<body>`, so this runs first.
/// An unterminated block drops the rest of the input.
fn drop_blocks(html: &str) -> String {
    let lower = html.to_ascii_lowercase(); // same byte offsets as `html`
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    while at < html.len() {
        let rest = &lower[at..];
        let next = ["<!--", "<style", "<script"]
            .iter()
            .filter_map(|open| rest.find(open).map(|i| (i, *open)))
            .min_by_key(|(i, _)| *i);
        let Some((i, open)) = next else {
            out.push_str(&html[at..]);
            break;
        };
        out.push_str(&html[at..at + i]);
        let close = match open {
            "<!--" => "-->",
            "<style" => "</style",
            _ => "</script",
        };
        let from = at + i + open.len();
        match lower[from..].find(close) {
            Some(j) => {
                let after_close = from + j + close.len();
                // For tags, skip to the end of the closing tag (`</style >`).
                at = if close == "-->" {
                    after_close
                } else {
                    lower[after_close..]
                        .find('>')
                        .map_or(html.len(), |k| after_close + k + 1)
                };
            }
            None => break,
        }
    }
    out
}

/// Marks around a link's number while the markup is reduced (private-use characters).
const MARK_OPEN: char = '\u{E000}';
const MARK_CLOSE: char = '\u{E001}';

/// Replace every `<a href="URL">text</a>` by `text` followed by a marked slot, and return the
/// URLs. Only plain `http(s)` addresses without whitespace or control characters are kept.
fn take_anchors(html: &str) -> (String, Vec<String>) {
    let lower = html.to_ascii_lowercase(); // same byte offsets as `html`
    let mut out = String::with_capacity(html.len());
    let mut urls = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<a ") {
        let open = at + i;
        let Some(gt) = lower[open..].find('>').map(|k| open + k) else {
            break;
        };
        out.push_str(&html[at..open]);
        let tag = &html[open..gt];
        let url = href(tag, &lower[open..gt]);
        let (inner_end, next) = match lower[gt..].find("</a") {
            Some(k) => {
                let close = gt + k;
                (
                    close,
                    lower[close..]
                        .find('>')
                        .map_or(html.len(), |m| close + m + 1),
                )
            }
            None => (html.len(), html.len()),
        };
        out.push_str(&html[gt + 1..inner_end]);
        if let Some(url) = url {
            out.push(MARK_OPEN);
            out.push_str(&urls.len().to_string());
            out.push(MARK_CLOSE);
            urls.push(url);
        }
        at = next;
    }
    out.push_str(&html[at.min(html.len())..]);
    (out, urls)
}

/// The `href` value of an opening tag, when it is a plain web address.
fn href(tag: &str, lower_tag: &str) -> Option<String> {
    let at = lower_tag.find("href")? + 4;
    let rest = tag[at..].trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let value = &rest[1..];
    let value = &value[..value.find(quote)?];
    let url = value.replace("&amp;", "&");
    let lower = url.to_ascii_lowercase();
    ((lower.starts_with("https://") || lower.starts_with("http://"))
        && url.len() > 8
        && !url.chars().any(|c| c.is_control() || c.is_whitespace()))
    .then_some(url)
}

/// Reduce markup to text: style/script/comment blocks dropped, tags removed, entities decoded.
/// A link keeps its address: `text (https://…)`, unless the text already is the address.
#[must_use]
pub fn to_text(html: &str) -> String {
    let (marked, urls) = take_anchors(&drop_blocks(html));
    let mut text = mail_parser::decoders::html::html_to_text(&marked)
        .trim()
        .to_owned();
    for (n, url) in urls.iter().enumerate() {
        let slot = format!("{MARK_OPEN}{n}{MARK_CLOSE}");
        let Some(at) = text.find(&slot) else { continue };
        let before = &text[..at];
        let repeated = before.trim_end().ends_with(url.as_str());
        let replacement = if repeated {
            String::new()
        } else {
            format!(" ({url})")
        };
        text.replace_range(at..at + slot.len(), &replacement);
    }
    text.replace([MARK_OPEN, MARK_CLOSE], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_keep_their_address_next_to_the_text() {
        let html = r#"<p>Go <a class="x" href="https://x.org/a?b=1&amp;c=2">Aktiválás</a> now,
            <a href="https://y.org/p">https://y.org/p</a> and <a href="javascript:alert(1)">bad</a>
            <a href='http://z.io/q'><img src="i.png"></a></p>"#;
        let text = to_text(html);
        assert!(
            text.contains("Aktiválás (https://x.org/a?b=1&c=2)"),
            "{text}"
        );
        assert_eq!(text.matches("https://y.org/p").count(), 1, "{text}");
        assert!(
            text.contains("bad") && !text.contains("javascript"),
            "{text}"
        );
        assert!(text.contains("(http://z.io/q)"), "{text}");
        assert!(!text.contains('\u{E000}'));
    }

    #[test]
    fn drop_blocks_is_safe_on_non_ascii_text() {
        assert_eq!(drop_blocks("é<style>x</style>ü<!--c-->ß"), "éüß");
        assert_eq!(drop_blocks("plain"), "plain");
    }
}
