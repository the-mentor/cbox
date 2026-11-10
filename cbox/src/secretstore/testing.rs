//! One in-memory `SecretStore` for tests: seeded entries, a `get_many` call counter, and a switch
//! that makes `get_many` return too few results.

use super::{SecretStore, StoredSecret};
use anyhow::Result;
use std::cell::Cell;
use std::collections::HashMap;

#[derive(Default)]
pub struct FakeStore {
    entries: HashMap<String, StoredSecret>,
    short: bool,
    calls: Cell<usize>,
}

impl FakeStore {
    pub fn with(name: &str, value: &str, hosts: &[&str], env: &str) -> Self {
        let mut s = Self::default();
        s.insert(name, value, hosts, env);
        s
    }

    pub fn insert(&mut self, name: &str, value: &str, hosts: &[&str], env: &str) {
        self.entries.insert(name.into(), StoredSecret { value: value.into(), hosts: hosts.iter().map(|h| h.to_string()).collect(), env: Some(env.into()), ..Default::default() });
    }

    /// A `--git` secret: `env` may be `None` (git only).
    pub fn insert_git(&mut self, name: &str, value: &str, hosts: &[&str], env: Option<&str>, user: &str) {
        self.entries.insert(name.into(), StoredSecret { value: value.into(), hosts: hosts.iter().map(|h| h.to_string()).collect(), env: env.map(String::from), user: Some(user.into()), git: true });
    }

    /// `get_many` will return no results at all, whatever the names.
    pub fn short() -> Self {
        Self { short: true, ..Self::default() }
    }

    pub fn get_many_calls(&self) -> usize {
        self.calls.get()
    }

    fn lookup(&self, name: &str) -> Option<StoredSecret> {
        self.entries.get(name).cloned()
    }
}

impl SecretStore for FakeStore {
    fn label(&self) -> &'static str {
        "fake"
    }
    fn max_entry_bytes(&self) -> Option<usize> {
        None
    }
    fn get(&self, name: &str) -> Result<Option<StoredSecret>> {
        Ok(self.lookup(name))
    }
    fn get_many(&self, names: &[&str]) -> Vec<Result<Option<StoredSecret>>> {
        self.calls.set(self.calls.get() + 1);
        if self.short {
            return vec![];
        }
        names.iter().map(|n| Ok(self.lookup(n))).collect()
    }
    fn put(&self, _: &str, _: &StoredSecret) -> Result<()> {
        unimplemented!()
    }
    fn delete(&self, _: &str) -> Result<bool> {
        unimplemented!()
    }
}
