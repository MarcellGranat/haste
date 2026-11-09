//! Non-secret settings (`gcal.json`) and the OAuth client credentials kept in the Keychain.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use haste_google_oauth::keychain::TokenStore;
use serde::{Deserialize, Serialize};

use crate::consts::CLIENT_ACCOUNT;

/// `gcal.json`: which account and which calendars. It never holds a token or a secret.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GcalConfig {
    /// The Google account address; also the Keychain account of the refresh token.
    #[serde(default)]
    pub account: String,
    /// Calendar ids to read. A missing or empty list reads nothing.
    #[serde(default)]
    pub calendars: Vec<String>,
}

impl GcalConfig {
    #[must_use]
    pub fn path(base: &Path) -> PathBuf {
        base.join("gcal.json")
    }

    /// # Errors
    /// I/O or JSON errors as text.
    pub fn load(base: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(Self::path(base))
            .map_err(|e| format!("cannot read gcal.json: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("invalid gcal.json: {e}"))
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
    pub fn selects(&self, calendar: &str) -> bool {
        self.calendars.iter().any(|c| c == calendar)
    }
}

/// The Google OAuth client. Stored in the Keychain (not in `gcal.json`) next to, but apart from,
/// the refresh token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientCredentials {
    pub client_id: String,
    pub client_secret: String,
}

impl ClientCredentials {
    /// # Errors
    /// The store's reason.
    pub fn save(&self, store: &dyn TokenStore) -> Result<(), String> {
        store.set(
            CLIENT_ACCOUNT,
            &serde_json::to_string(self).map_err(|e| e.to_string())?,
        )
    }

    /// # Errors
    /// The store's reason, or a message when nothing is stored yet.
    pub fn load(store: &dyn TokenStore) -> Result<Self, String> {
        let raw = store
            .get(CLIENT_ACCOUNT)?
            .ok_or_else(|| "no OAuth client stored; run `haste connector gcal auth`".to_owned())?;
        serde_json::from_str(&raw).map_err(|_| "stored OAuth client is unreadable".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haste_google_oauth::keychain::MemoryStore;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_settings_file_is_private_secret_free_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let config = GcalConfig {
            account: "me@example.org".into(),
            calendars: vec!["primary".into(), "team@group.calendar.google.com".into()],
        };
        config.save(dir.path()).unwrap();
        let mode = fs::metadata(GcalConfig::path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let text = fs::read_to_string(GcalConfig::path(dir.path())).unwrap();
        assert!(!text.contains("secret") && !text.contains("token"));
        assert_eq!(GcalConfig::load(dir.path()).unwrap(), config);
        assert!(config.selects("primary") && !config.selects("other"));
    }

    #[test]
    fn a_missing_calendar_list_selects_nothing() {
        let config: GcalConfig = serde_json::from_str(r#"{"account":"a@b"}"#).unwrap();
        assert!(config.calendars.is_empty() && !config.selects("primary"));
    }

    #[test]
    fn client_credentials_live_under_their_own_keychain_account() {
        let store = MemoryStore::default();
        assert!(ClientCredentials::load(&store).is_err());
        let creds = ClientCredentials {
            client_id: "id".into(),
            client_secret: "s".into(),
        };
        creds.save(&store).unwrap();
        assert_eq!(ClientCredentials::load(&store).unwrap(), creds);
        // the refresh-token account (the address) is a different item
        assert_eq!(store.get("me@example.org").unwrap(), None);
    }
}
