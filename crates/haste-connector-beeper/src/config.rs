//! `beeper.json`: non-secret selection. Nothing is read unless it is named here. The API token
//! is never in this file (it lives in the Keychain).

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::consts::{BEEPER_BASE_URL, BEEPER_DEFAULT_HISTORY_DAYS, BEEPER_MAX_HISTORY_DAYS};

fn default_history() -> u32 {
    BEEPER_DEFAULT_HISTORY_DAYS
}

/// One selected Beeper account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AccountSelection {
    pub id: String,
    /// The human states this account's connection is on-device. Used only when the API does not
    /// say; a connection the API reports as cloud is never made on-device by this.
    #[serde(default)]
    pub on_device_attested: bool,
    /// The human accepts that this account runs in Beeper's cloud (its messages pass through
    /// Beeper's servers) and wants it used anyway. Per account, off unless written here.
    #[serde(default)]
    pub allow_cloud: bool,
}

/// Which chats are read. `deny` wins over `allow`; an empty `allow` reads nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChatSelection {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BeeperConfig {
    #[serde(default)]
    pub accounts: Vec<AccountSelection>,
    #[serde(default)]
    pub chats: ChatSelection,
    #[serde(default = "default_history")]
    pub history_days: u32,
    /// Overrides the base URL; only loopback addresses are accepted.
    #[serde(default)]
    pub base_url: Option<String>,
}

impl Default for BeeperConfig {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            chats: ChatSelection::default(),
            history_days: default_history(),
            base_url: None,
        }
    }
}

/// A base URL must be plain `http` on a loopback host with no credentials, path or query, so the
/// token can only ever go to this machine.
///
/// # Errors
/// A fixed reason.
pub fn validate_base_url(url: &str) -> Result<(), &'static str> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("base url must be http on loopback")?;
    if rest.contains(['@', '?', '#', '\\']) {
        return Err("base url must not carry credentials or a query");
    }
    let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
    if !path.is_empty() {
        return Err("base url must not have a path");
    }
    let host = if let Some(v6) = host_port.strip_prefix('[') {
        v6.split_once(']').map_or("", |(h, _)| h)
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    if matches!(host, "127.0.0.1" | "localhost" | "::1") {
        Ok(())
    } else {
        Err("base url must be a loopback address")
    }
}

impl BeeperConfig {
    #[must_use]
    pub fn path(base: &Path) -> PathBuf {
        base.join("beeper.json")
    }

    /// Check the settings (loopback URL, history bound).
    ///
    /// # Errors
    /// A fixed reason.
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(url) = &self.base_url {
            validate_base_url(url)?;
        }
        if self.history_days == 0 || self.history_days > BEEPER_MAX_HISTORY_DAYS {
            return Err("history_days must be between 1 and 90");
        }
        Ok(())
    }

    /// The base URL to use (validated).
    #[must_use]
    pub fn base(&self) -> &str {
        self.base_url.as_deref().unwrap_or(BEEPER_BASE_URL)
    }

    /// Load and validate. A missing file is the empty selection (nothing is read).
    ///
    /// # Errors
    /// Unreadable file, invalid JSON, an unknown key, or a failed validation.
    pub fn load(base: &Path) -> Result<Self, String> {
        let text = match fs::read_to_string(Self::path(base)) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("cannot read beeper.json: {e}")),
        };
        let config: Self =
            serde_json::from_str(&text).map_err(|e| format!("invalid beeper.json: {e}"))?;
        config
            .validate()
            .map_err(|why| format!("invalid beeper.json: {why}"))?;
        Ok(config)
    }

    /// Write with mode `0600`.
    ///
    /// # Errors
    /// I/O errors as text.
    pub fn save(&self, base: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(Self::path(base))
            .map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())
    }

    #[must_use]
    pub fn account(&self, id: &str) -> Option<&AccountSelection> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// Whether a chat may be read or replied to: allowed, and not denied.
    #[must_use]
    pub fn selects_chat(&self, chat_id: &str) -> bool {
        self.chats.allow.iter().any(|c| c == chat_id)
            && !self.chats.deny.iter().any(|c| c == chat_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn only_loopback_http_urls_are_accepted() {
        for ok in [
            "http://127.0.0.1:23373",
            "http://localhost:23373",
            "http://[::1]:23373",
            "http://localhost",
        ] {
            assert_eq!(validate_base_url(ok), Ok(()), "{ok}");
        }
        for bad in [
            "https://127.0.0.1:23373",
            "http://example.com:23373",
            "http://127.0.0.1.evil.com:23373",
            "http://user:pw@127.0.0.1:23373",
            "http://127.0.0.1:23373/v1",
            "http://127.0.0.1:23373?x=1",
            "http://192.168.1.5:23373",
            "http://0.0.0.0:23373",
            "ftp://127.0.0.1",
            "",
        ] {
            assert!(validate_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn nothing_is_selected_by_default_and_deny_wins_over_allow() {
        let c = BeeperConfig::default();
        assert!(!c.selects_chat("any"));
        let c = BeeperConfig {
            chats: ChatSelection {
                allow: vec!["a".into(), "b".into()],
                deny: vec!["b".into()],
            },
            ..BeeperConfig::default()
        };
        assert!(c.selects_chat("a") && !c.selects_chat("b") && !c.selects_chat("c"));
    }

    #[test]
    fn the_file_is_private_validated_and_rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            BeeperConfig::load(dir.path()).unwrap(),
            BeeperConfig::default()
        );
        let c = BeeperConfig {
            accounts: vec![AccountSelection {
                id: "acc".into(),
                on_device_attested: true,
                allow_cloud: true,
            }],
            chats: ChatSelection {
                allow: vec!["c1".into()],
                deny: vec![],
            },
            history_days: 14,
            base_url: None,
        };
        c.save(dir.path()).unwrap();
        let mode = fs::metadata(BeeperConfig::path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let text = fs::read_to_string(BeeperConfig::path(dir.path()))
            .unwrap()
            .to_lowercase();
        assert!(!text.contains("token") && !text.contains("secret"));
        assert_eq!(BeeperConfig::load(dir.path()).unwrap(), c);
        for bad in [
            r#"{"token":"x"}"#,
            r#"{"base_url":"http://example.com"}"#,
            r#"{"history_days":0}"#,
            r#"{"history_days":91}"#,
            r#"{"accounts":[{"id":"a","cloud":true}]}"#,
        ] {
            fs::write(BeeperConfig::path(dir.path()), bad).unwrap();
            assert!(BeeperConfig::load(dir.path()).is_err(), "{bad}");
        }
    }
}
