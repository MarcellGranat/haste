//! The emoji keyboard: every letter and digit key but `y` and `z` stands for one emoji. Ctrl-o
//! in a text field and Ctrl-e on a chat message open it; pressing a key picks its emoji.

/// The keyboard rows, as laid out on a QWERTY keyboard without `y` and `z`, each key with its
/// emoji. The first eight digits keep the order of the old reaction picker.
pub const ROWS: [&[(char, &str)]; 4] = [
    &[
        ('1', "👍"),
        ('2', "❤️"),
        ('3', "😂"),
        ('4', "😮"),
        ('5', "😢"),
        ('6', "🙏"),
        ('7', "🔥"),
        ('8', "🎉"),
        ('9', "👀"),
        ('0', "✅"),
    ],
    &[
        ('q', "❓"),
        ('w', "👋"),
        ('e', "😍"),
        ('r', "🚀"),
        ('t', "🤝"),
        ('u', "🆙"),
        ('i', "💡"),
        ('o', "👌"),
        ('p', "🎊"),
    ],
    &[
        ('a', "😅"),
        ('s', "😴"),
        ('d', "💪"),
        ('f', "🙌"),
        ('g', "😁"),
        ('h', "🤗"),
        ('j', "🤣"),
        ('k', "💋"),
        ('l', "🥰"),
    ],
    &[
        ('x', "❌"),
        ('c', "👏"),
        ('v', "✌️"),
        ('b', "🍺"),
        ('n', "🙅"),
        ('m', "💰"),
    ],
];

/// The emoji of a key (letters in either case, digits), if it has one.
#[must_use]
pub fn emoji_for(key: char) -> Option<&'static str> {
    let key = key.to_ascii_lowercase();
    ROWS.iter()
        .flat_map(|row| row.iter())
        .find(|(k, _)| *k == key)
        .map(|(_, e)| *e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_letter_and_digit_but_y_and_z_has_its_own_emoji() {
        let all: Vec<(char, &str)> = ROWS.iter().flat_map(|r| r.iter().copied()).collect();
        let keys: std::collections::BTreeSet<char> = all.iter().map(|(k, _)| *k).collect();
        let expected: std::collections::BTreeSet<char> = ('a'..='z')
            .filter(|c| !matches!(c, 'y' | 'z'))
            .chain('0'..='9')
            .collect();
        assert_eq!(keys, expected);
        assert_eq!(all.len(), 34);
        let emoji: std::collections::BTreeSet<&str> = all.iter().map(|(_, e)| *e).collect();
        assert_eq!(emoji.len(), all.len(), "no two keys share an emoji");
        assert!(all.iter().all(|(_, e)| haste_core::reactions::valid_key(e)));
        assert_eq!(emoji_for('Q'), Some("❓"));
        assert_eq!(emoji_for('1'), Some("👍"));
        assert_eq!(emoji_for('y'), None);
        assert_eq!(emoji_for('z'), None);
        assert_eq!(emoji_for('-'), None);
    }
}
