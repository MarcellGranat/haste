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

/// Reduce markup to text: style/script/comment blocks dropped, tags removed, entities decoded.
#[must_use]
pub fn to_text(html: &str) -> String {
    mail_parser::decoders::html::html_to_text(&drop_blocks(html))
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_blocks_is_safe_on_non_ascii_text() {
        assert_eq!(drop_blocks("é<style>x</style>ü<!--c-->ß"), "éüß");
        assert_eq!(drop_blocks("plain"), "plain");
    }
}
