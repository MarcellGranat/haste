//! The one-time authorization step, shared by the helper binary and the tests.
//! The app-specific password is validated against the server first and only then stored.

use std::path::Path;

use crate::config::{IcloudConfig, DEFAULT_FOLDER};
use crate::secrets::SecretStore;
use crate::session::{ImapFailure, Opener};

/// LOGIN and EXAMINE INBOX must both succeed before anything is written. The Keychain item is
/// written before `icloud.json`, so a failure never leaves settings without a password.
///
/// # Errors
/// A message safe to show: it never contains the password or server text.
pub fn authorize(
    opener: &dyn Opener,
    secrets: &dyn SecretStore,
    base: &Path,
    config: &IcloudConfig,
    password: &str,
) -> Result<(), String> {
    let mut session = opener.open(config, password).map_err(|f| match f {
        ImapFailure::Auth => {
            "iCloud rejected the address or app-specific password; nothing was stored".to_owned()
        }
        ImapFailure::Transient(m) => format!("could not reach iCloud ({m}); nothing was stored"),
        ImapFailure::Protocol(m) => format!("unexpected server response ({m}); nothing was stored"),
    })?;
    let inbox = session.examine(DEFAULT_FOLDER);
    session.logout();
    inbox.map_err(|_| "logged in, but INBOX could not be opened; nothing was stored".to_owned())?;
    secrets.set(&config.address, password)?;
    config.save(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;
    use crate::session::PlainOpener;
    use crate::testing::{spawn, FakeFolder, FakeState};

    fn fake(password: &str, with_inbox: bool) -> crate::testing::FakeImap {
        let mut state = FakeState {
            password: password.into(),
            ..FakeState::default()
        };
        if with_inbox {
            state.folders.insert(
                "INBOX".into(),
                FakeFolder {
                    uid_validity: 1,
                    messages: vec![],
                },
            );
        }
        spawn(state)
    }

    fn config(port: u16) -> IcloudConfig {
        let mut c = IcloudConfig::new("me@icloud.com");
        c.host = "127.0.0.1".into();
        c.port = port;
        c
    }

    #[test]
    fn success_stores_the_password_in_the_secret_store_and_settings_without_it() {
        let dir = tempfile::tempdir().unwrap();
        let server = fake("good-password", true);
        let secrets = MemoryStore::default();
        authorize(
            &PlainOpener,
            &secrets,
            dir.path(),
            &config(server.port),
            "good-password",
        )
        .unwrap();
        assert_eq!(
            secrets.get("me@icloud.com").unwrap().as_deref(),
            Some("good-password")
        );
        let text = std::fs::read_to_string(IcloudConfig::path(dir.path())).unwrap();
        assert!(!text.contains("good-password"));
    }

    #[test]
    fn a_rejected_login_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let server = fake("good-password", true);
        let secrets = MemoryStore::default();
        let err = authorize(
            &PlainOpener,
            &secrets,
            dir.path(),
            &config(server.port),
            "typo",
        )
        .unwrap_err();
        assert!(err.contains("nothing was stored") && !err.contains("typo"));
        assert_eq!(secrets.get("me@icloud.com").unwrap(), None);
        assert!(!IcloudConfig::path(dir.path()).exists());
    }

    #[test]
    fn an_unreadable_inbox_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let server = fake("good-password", false);
        let secrets = MemoryStore::default();
        assert!(authorize(
            &PlainOpener,
            &secrets,
            dir.path(),
            &config(server.port),
            "good-password"
        )
        .is_err());
        assert_eq!(secrets.get("me@icloud.com").unwrap(), None);
        assert!(!IcloudConfig::path(dir.path()).exists());
    }
}
