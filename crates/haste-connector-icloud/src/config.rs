//! Non-secret settings in `icloud.json` (mode 0600). The password is never stored here.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_HOST: &str = "imap.mail.me.com";
pub const DEFAULT_PORT: u16 = 993;
pub const DEFAULT_FOLDER: &str = "INBOX";
/// Where mail is submitted (STARTTLS on the submission port).
pub const DEFAULT_SMTP_HOST: &str = "smtp.mail.me.com";
pub const DEFAULT_SMTP_PORT: u16 = 587;
/// Where a copy of a sent message is filed over IMAP.
pub const DEFAULT_SENT_FOLDER: &str = "Sent Messages";
/// Newest messages fetched by a first sync (or after a UIDVALIDITY change) unless configured.
pub const DEFAULT_BACKFILL: usize = 200;
/// Seconds between scheduled syncs.
pub const ICLOUD_SYNC_INTERVAL_SECS: u64 = 300;

fn default_host() -> String {
    DEFAULT_HOST.to_owned()
}

const fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_smtp_host() -> String {
    DEFAULT_SMTP_HOST.to_owned()
}

const fn default_smtp_port() -> u16 {
    DEFAULT_SMTP_PORT
}

fn default_sent_folder() -> String {
    DEFAULT_SENT_FOLDER.to_owned()
}

const fn default_true() -> bool {
    true
}

const fn default_backfill() -> usize {
    DEFAULT_BACKFILL
}

/// One folder the user wants synced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FolderConfig {
    pub name: String,
    /// Overrides the global `backfill_count` for this folder.
    #[serde(default)]
    pub backfill: Option<usize>,
}

impl FolderConfig {
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            backfill: None,
        }
    }
}

/// Everything except the app-specific password, which lives in the Keychain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IcloudConfig {
    /// The Apple ID address; also the Keychain account name.
    pub address: String,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Folders to sync. INBOX only when empty or absent.
    #[serde(default)]
    pub folders: Vec<FolderConfig>,
    /// Global default for the first-sync size.
    #[serde(default = "default_backfill")]
    pub backfill_count: usize,
    #[serde(default = "default_smtp_host")]
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// The folder a copy of each sent message is filed in.
    #[serde(default = "default_sent_folder")]
    pub sent_folder: String,
    /// File a copy in `sent_folder` after a successful send. Turn off if iCloud already files
    /// the submissions itself (the live spike decides), or you would get duplicates.
    #[serde(default = "default_true")]
    pub append_sent: bool,
}

impl IcloudConfig {
    #[must_use]
    pub fn new(address: &str) -> Self {
        Self {
            address: address.to_owned(),
            host: default_host(),
            port: default_port(),
            folders: Vec::new(),
            backfill_count: default_backfill(),
            smtp_host: default_smtp_host(),
            smtp_port: default_smtp_port(),
            sent_folder: default_sent_folder(),
            append_sent: true,
        }
    }

    /// The folders to sync: the user's list, or INBOX.
    #[must_use]
    pub fn effective_folders(&self) -> Vec<FolderConfig> {
        if self.folders.is_empty() {
            vec![FolderConfig::new(DEFAULT_FOLDER)]
        } else {
            self.folders.clone()
        }
    }

    /// First-sync size for a folder: its own setting, else the global one.
    #[must_use]
    pub fn backfill_for(&self, folder: &str) -> usize {
        self.folders
            .iter()
            .find(|f| f.name == folder)
            .and_then(|f| f.backfill)
            .unwrap_or(self.backfill_count)
    }

    #[must_use]
    pub fn path(base: &Path) -> PathBuf {
        base.join("icloud.json")
    }

    /// # Errors
    /// I/O or JSON errors as text.
    pub fn load(base: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(Self::path(base))
            .map_err(|e| format!("cannot read icloud.json: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("invalid icloud.json: {e}"))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn defaults_are_inbox_and_two_hundred() {
        let c = IcloudConfig::new("me@icloud.com");
        assert_eq!(c.host, "imap.mail.me.com");
        assert_eq!(c.port, 993);
        assert_eq!(c.effective_folders(), vec![FolderConfig::new("INBOX")]);
        assert_eq!(c.backfill_for("INBOX"), 200);
    }

    #[test]
    fn user_folders_and_backfill_sizes_are_honoured() {
        let mut c = IcloudConfig::new("me@icloud.com");
        c.backfill_count = 50;
        c.folders = vec![
            FolderConfig::new("INBOX"),
            FolderConfig {
                name: "Sent Messages".into(),
                backfill: Some(1000),
            },
        ];
        assert_eq!(c.effective_folders().len(), 2);
        assert_eq!(c.backfill_for("INBOX"), 50);
        assert_eq!(c.backfill_for("Sent Messages"), 1000);
        assert_eq!(c.backfill_for("Unlisted"), 50);
    }

    #[test]
    fn send_settings_default_to_icloud_submission_and_a_sent_copy() {
        let c: IcloudConfig = serde_json::from_str(r#"{"address":"me@icloud.com"}"#).unwrap();
        assert_eq!(
            (c.smtp_host.as_str(), c.smtp_port),
            ("smtp.mail.me.com", 587)
        );
        assert_eq!(c.sent_folder, "Sent Messages");
        assert!(c.append_sent);
        let off: IcloudConfig =
            serde_json::from_str(r#"{"address":"a@b","append_sent":false,"sent_folder":"Sent"}"#)
                .unwrap();
        assert!(!off.append_sent);
        assert_eq!(off.sent_folder, "Sent");
    }

    #[test]
    fn minimal_json_fills_in_defaults() {
        let c: IcloudConfig = serde_json::from_str(r#"{"address":"me@icloud.com"}"#).unwrap();
        assert_eq!(c, IcloudConfig::new("me@icloud.com"));
    }

    #[test]
    fn file_is_private_and_holds_no_secret_field() {
        let dir = tempfile::tempdir().unwrap();
        let c = IcloudConfig::new("me@icloud.com");
        c.save(dir.path()).unwrap();
        let mode = fs::metadata(IcloudConfig::path(dir.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let text = fs::read_to_string(IcloudConfig::path(dir.path()))
            .unwrap()
            .to_lowercase();
        assert!(!text.contains("password") && !text.contains("secret") && !text.contains("token"));
        assert_eq!(IcloudConfig::load(dir.path()).unwrap(), c);
    }
}
