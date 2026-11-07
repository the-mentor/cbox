// KeyringStore is compiled everywhere but only used where a keyring store is registered (macOS).
#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

use super::StoredSecret;
use anyhow::{Context, Result, anyhow};
use keyring_core::{Entry, Error};

pub trait SecretStore {
    fn label(&self) -> &'static str;
    fn max_entry_bytes(&self) -> Option<usize>;
    fn get(&self, name: &str) -> Result<Option<StoredSecret>>;
    /// Whether an entry exists, without parsing it (a corrupt entry still counts).
    fn exists(&self, name: &str) -> Result<bool> {
        Ok(self.get(name)?.is_some())
    }
    fn put(&self, name: &str, s: &StoredSecret) -> Result<()>;
    /// Returns whether an entry existed.
    fn delete(&self, name: &str) -> Result<bool>;
}

pub struct KeyringStore {
    label: &'static str,
    limit: Option<usize>,
}

impl KeyringStore {
    pub fn new(label: &'static str, limit: Option<usize>) -> Self {
        Self { label, limit }
    }
}

fn entry(name: &str) -> Result<Entry> {
    Entry::new("cbox", name).map_err(|e| anyhow!("secret store error: {e}"))
}

impl SecretStore for KeyringStore {
    fn label(&self) -> &'static str {
        self.label
    }
    fn max_entry_bytes(&self) -> Option<usize> {
        self.limit
    }
    fn get(&self, name: &str) -> Result<Option<StoredSecret>> {
        match entry(name)?.get_password() {
            Ok(json) => Ok(Some(
                StoredSecret::from_json(&json).with_context(|| format!("secret {name:?} in {}", self.label))?,
            )),
            Err(Error::NoEntry | Error::NoDefaultStore) => Ok(None),
            Err(e) => Err(anyhow!("reading secret {name:?} from {}: {e}", self.label)),
        }
    }
    fn exists(&self, name: &str) -> Result<bool> {
        match entry(name)?.get_password() {
            Ok(_) => Ok(true),
            // An entry whose bytes are not UTF-8 still exists.
            Err(Error::BadEncoding(_)) => Ok(true),
            Err(Error::NoEntry | Error::NoDefaultStore) => Ok(false),
            Err(e) => Err(anyhow!("reading secret {name:?} from {}: {e}", self.label)),
        }
    }
    fn put(&self, name: &str, s: &StoredSecret) -> Result<()> {
        entry(name)?
            .set_password(&s.to_json())
            .map_err(|e| anyhow!("writing secret {name:?} to {}: {e}", self.label))
    }
    fn delete(&self, name: &str) -> Result<bool> {
        match entry(name)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(Error::NoEntry) => Ok(false),
            Err(e) => Err(anyhow!("deleting secret {name:?} from {}: {e}", self.label)),
        }
    }
}

#[cfg(target_os = "macos")]
pub fn open_default() -> Result<Box<dyn SecretStore>> {
    let store = apple_native_keyring_store::keychain::Store::new() // Result<Arc<Store>, keyring_core::Error>
        .map_err(|e| anyhow!("cannot open macOS Keychain: {e}"))?;
    keyring_core::set_default_store(store);
    Ok(Box::new(KeyringStore::new("macOS Keychain", None)))
}

#[cfg(not(target_os = "macos"))]
pub fn open_default() -> Result<Box<dyn SecretStore>> {
    #[cfg(target_os = "linux")]
    if super::wincred::is_wsl() {
        return Ok(Box::new(super::wincred::WinCredStore));
    }
    anyhow::bail!(
        "no secret store available on this platform (macOS Keychain and Windows Credential Manager under WSL are supported; Linux desktop is planned)"
    )
}
