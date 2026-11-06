use super::StoredSecret;
use anyhow::{Context, Result, anyhow};
use keyring_core::{Entry, Error};

pub trait SecretStore {
    fn label(&self) -> &'static str;
    fn max_entry_bytes(&self) -> Option<usize>;
    fn get(&self, name: &str) -> Result<Option<StoredSecret>>;
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
    anyhow::bail!(
        "no secret store available on this platform (macOS Keychain is supported today; Windows/WSL and Linux desktop are planned)"
    )
}
