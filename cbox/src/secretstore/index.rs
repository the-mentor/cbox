use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// Names only; values and hosts live in the secret store.
#[derive(Default, Serialize, Deserialize)]
pub struct Index {
    pub names: BTreeSet<String>,
}

impl Index {
    pub fn load(path: &Path) -> Result<Index> {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).with_context(|| format!("invalid index {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Index::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        use std::io::Write;
        let dir = path.parent().context("index path has no parent")?;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let tmp = path.with_extension("json.tmp");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(serde_json::to_string(self)?.as_bytes())?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
    }

    pub fn insert(&mut self, name: &str) {
        self.names.insert(name.to_string());
    }

    pub fn remove(&mut self, name: &str) {
        self.names.remove(name);
    }
}
