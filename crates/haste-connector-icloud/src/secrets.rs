//! The app-specific password lives only in the macOS Keychain (service `com.haste.icloud`).

use std::collections::HashMap;
use std::sync::Mutex;

use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

pub const KEYCHAIN_SERVICE: &str = "com.haste.icloud";
/// `errSecItemNotFound`
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

pub trait SecretStore: Send + Sync {
    /// # Errors
    /// A human-readable reason; never includes the secret.
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    /// # Errors
    /// A human-readable reason.
    fn set(&self, account: &str, secret: &str) -> Result<(), String>;
    /// # Errors
    /// A human-readable reason.
    fn delete(&self, account: &str) -> Result<(), String>;
}

/// Generic-password item, account = the Apple ID address.
#[derive(Default)]
pub struct KeychainStore;

impl SecretStore for KeychainStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match get_generic_password(KEYCHAIN_SERVICE, account) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(format!("keychain read failed ({})", e.code())),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        set_generic_password(KEYCHAIN_SERVICE, account, secret.as_bytes())
            .map_err(|e| format!("keychain write failed ({})", e.code()))
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        match delete_generic_password(KEYCHAIN_SERVICE, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(format!("keychain delete failed ({})", e.code())),
        }
    }
}

/// In-memory fake for tests.
#[derive(Default)]
pub struct MemoryStore {
    items: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self
            .items
            .lock()
            .map_err(|_| "poisoned")?
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        self.items
            .lock()
            .map_err(|_| "poisoned")?
            .insert(account.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.items.lock().map_err(|_| "poisoned")?.remove(account);
        Ok(())
    }
}
