//! The local mute list: `.haste_ignore` in the data folder, one regular expression per line
//! (blank lines and lines starting with `#` are skipped), matched case-insensitively against a
//! thread's senders. It only hides threads in the TUI: nothing is changed in any mailbox or chat
//! app, and agents are not affected.

use regex::{Regex, RegexBuilder};

pub const IGNORE_FILE: &str = ".haste_ignore";
/// Senders whose mail is priority, in the data folder (same line format as the mute list).
pub const PRIORITY_FILE: &str = "priority_senders";
/// Senders let through although none of their mail was read yet.
pub const ALLOWED_FILE: &str = "allowed_senders";
/// Patterns read; further lines are ignored.
pub const MAX_PATTERNS: usize = 1000;
/// Longest pattern line accepted.
pub const MAX_PATTERN_LEN: usize = 200;
/// Compiled size limit per pattern, so a hostile pattern cannot use much memory.
const REGEX_SIZE_LIMIT: usize = 100_000;

#[derive(Debug, Clone, Default)]
pub struct IgnoreList {
    patterns: Vec<(String, Regex)>,
}

impl IgnoreList {
    /// Parse the file text. Returns the list and one message per unusable line (line number and
    /// reason only, never the pattern).
    #[must_use]
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut list = Self::default();
        let mut errors = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if list.patterns.len() >= MAX_PATTERNS {
                errors.push(format!("line {}: more than {MAX_PATTERNS} patterns", i + 1));
                break;
            }
            match compile(line) {
                Ok(re) => list.patterns.push((line.to_owned(), re)),
                Err(why) => errors.push(format!("line {}: {why}", i + 1)),
            }
        }
        (list, errors)
    }

    /// Build from already-validated pattern lines (the daemon's answer).
    #[must_use]
    pub fn from_patterns(patterns: &[String]) -> Self {
        let (list, _) = Self::parse(&patterns.join("\n"));
        list
    }

    #[must_use]
    pub fn patterns(&self) -> Vec<String> {
        self.patterns.iter().map(|(p, _)| p.clone()).collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Whether any pattern matches any of the senders.
    #[must_use]
    pub fn is_muted<S: AsRef<str>>(&self, senders: &[S]) -> bool {
        self.patterns
            .iter()
            .any(|(_, re)| senders.iter().any(|s| re.is_match(s.as_ref())))
    }
}

fn compile(pattern: &str) -> Result<Regex, &'static str> {
    if pattern.len() > MAX_PATTERN_LEN {
        return Err("pattern too long");
    }
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|_| "not a valid regular expression")
}

/// The bare lowercase address of a sender: the part in `<...>` when there is one.
#[must_use]
pub fn address_of(sender: &str) -> String {
    let s = sender.trim();
    match (s.rfind('<'), s.rfind('>')) {
        (Some(a), Some(b)) if a < b => s[a + 1..b].trim().to_lowercase(),
        _ => s.to_lowercase(),
    }
}

/// The line that matches every sender at this sender's domain.
#[must_use]
pub fn domain_pattern_for(sender: &str) -> Option<String> {
    let address = address_of(sender);
    let (_, domain) = address.rsplit_once('@')?;
    (!domain.is_empty()).then(|| format!("@{}$", regex::escape(domain)))
}

/// The line that mutes exactly this sender.
#[must_use]
pub fn pattern_for(sender: &str) -> String {
    format!("^{}$", regex::escape(sender.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_domains_are_taken_from_senders() {
        assert_eq!(address_of("Ana <Ana@X.org>"), "ana@x.org");
        assert_eq!(address_of(" B@y.com "), "b@y.com");
        let p = domain_pattern_for("Ana <ana@shop.example.com>").unwrap();
        let list = IgnoreList::from_patterns(&[p]);
        assert!(list.is_muted(&["bob@shop.example.com"]));
        assert!(!list.is_muted(&["bob@shop.example.com.evil.org", "bob@example.com"]));
        assert!(domain_pattern_for("no-at-sign").is_none());
    }

    #[test]
    fn patterns_match_senders_case_insensitively() {
        let (list, errors) =
            IgnoreList::parse("# newsletters\n\n^news@shop\\.com$\n@noisy\\.example$\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(list.is_muted(&["me@x.org", "News@Shop.com"]));
        assert!(list.is_muted(&["anyone@noisy.example"]));
        assert!(!list.is_muted(&["news@shop.com.evil.org", "ana@x.org"]));
    }

    #[test]
    fn bad_lines_are_reported_by_number_and_skipped() {
        let (list, errors) = IgnoreList::parse("ok\n(unclosed\n");
        assert_eq!(list.patterns(), vec!["ok".to_owned()]);
        assert_eq!(
            errors,
            vec!["line 2: not a valid regular expression".to_owned()]
        );
        assert!(!errors[0].contains("unclosed"), "the pattern is not echoed");
        let long = "a".repeat(MAX_PATTERN_LEN + 1);
        assert_eq!(IgnoreList::parse(&long).1.len(), 1);
    }

    #[test]
    fn the_generated_pattern_mutes_exactly_that_sender() {
        let p = pattern_for("A.B+c@x.org");
        let list = IgnoreList::from_patterns(&[p]);
        assert!(list.is_muted(&["a.b+c@x.org"]));
        assert!(!list.is_muted(&["aXb+c@x.org", "a.b+c@x.org.evil"]));
    }
}
