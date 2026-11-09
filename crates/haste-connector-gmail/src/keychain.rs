//! Refresh-token storage for Gmail: the shared store under the `com.haste.gmail` service.

pub use haste_google_oauth::keychain::{MemoryStore, TokenStore};

use crate::consts::KEYCHAIN_SERVICE;

/// Generic-password item under service `com.haste.gmail`, account = the Gmail address.
#[derive(Default)]
pub struct KeychainStore;

impl KeychainStore {
    fn inner() -> haste_google_oauth::keychain::KeychainStore {
        haste_google_oauth::keychain::KeychainStore::new(KEYCHAIN_SERVICE)
    }
}

impl TokenStore for KeychainStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Self::inner().get(account)
    }

    fn set(&self, account: &str, token: &str) -> Result<(), String> {
        Self::inner().set(account, token)
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        Self::inner().delete(account)
    }
}
