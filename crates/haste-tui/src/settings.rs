//! User settings from a TOML file (`~/.config/haste/config.toml`, or `$HASTE_CONFIG`).
//!
//! ```toml
//! [send]
//! chat_delay_secs = 5      # undo window after Enter in a chat (WhatsApp, Messenger, Google Chat, iMessage)
//! mail_delay_secs = 10     # undo window for mail
//! confirm_chat = false     # ask "Send this reply?" before a chat reply
//! confirm_mail = true
//!
//! [send.delay_secs]        # optional per-source override
//! googlechat = 3
//! ```
//!
//! A missing file means the defaults; a broken one keeps the defaults and reports why.
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The sources that are chats: short undo window, no confirmation by default.
pub const CHAT_SOURCES: [&str; 4] = ["imessage", "whatsapp", "messenger", "googlechat"];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SendSettings {
    pub chat_delay_secs: u64,
    pub mail_delay_secs: u64,
    pub confirm_chat: bool,
    pub confirm_mail: bool,
    pub delay_secs: HashMap<String, u64>,
}

impl Default for SendSettings {
    fn default() -> Self {
        Self {
            chat_delay_secs: 5,
            mail_delay_secs: 10,
            confirm_chat: false,
            confirm_mail: true,
            delay_secs: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub send: SendSettings,
}

impl Settings {
    /// The undo window for a send from `source`.
    #[must_use]
    pub fn send_delay(&self, source: &str) -> Duration {
        let secs = self.send.delay_secs.get(source).copied().unwrap_or(
            if CHAT_SOURCES.contains(&source) {
                self.send.chat_delay_secs
            } else {
                self.send.mail_delay_secs
            },
        );
        Duration::from_secs(secs)
    }

    /// Whether a reply from `source` asks "Send this reply?" first.
    #[must_use]
    pub fn confirm_reply(&self, source: &str) -> bool {
        if CHAT_SOURCES.contains(&source) {
            self.send.confirm_chat
        } else {
            self.send.confirm_mail
        }
    }

    /// Parse a TOML document.
    ///
    /// # Errors
    /// The parse error, naming the offending key or line.
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.message().to_owned())
    }

    /// Load the file at `path`: absent -> defaults; unreadable or invalid -> defaults and a warning.
    #[must_use]
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match Self::parse(&text) {
                Ok(s) => (s, None),
                Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
        }
    }

    /// `$HASTE_CONFIG`, else `$XDG_CONFIG_HOME/haste/config.toml`, else `~/.config/haste/config.toml`.
    #[must_use]
    pub fn path() -> PathBuf {
        if let Some(p) = std::env::var_os("HASTE_CONFIG") {
            return PathBuf::from(p);
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_default();
        base.join("haste/config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_five_seconds_for_chats_without_asking_and_ten_for_mail() {
        let s = Settings::default();
        assert_eq!(s.send_delay("messenger"), Duration::from_secs(5));
        assert_eq!(s.send_delay("gmail"), Duration::from_secs(10));
        assert!(!s.confirm_reply("googlechat"));
        assert!(s.confirm_reply("gmail"));
    }

    #[test]
    fn the_file_overrides_per_kind_and_per_source() {
        let s = Settings::parse(
            "[send]\nchat_delay_secs = 2\nconfirm_chat = true\n[send.delay_secs]\nwhatsapp = 8\n",
        )
        .unwrap();
        assert_eq!(s.send_delay("messenger"), Duration::from_secs(2));
        assert_eq!(s.send_delay("whatsapp"), Duration::from_secs(8));
        assert!(s.confirm_reply("imessage"));
    }

    #[test]
    fn unknown_keys_are_reported_and_a_missing_file_is_fine() {
        assert!(Settings::parse("[send]\nchat_dely_secs = 2\n").is_err());
        let (s, warn) = Settings::load(Path::new("/nonexistent/haste.toml"));
        assert_eq!((s, warn), (Settings::default(), None));
    }
}
