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
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        let tmp = path.with_extension("json.tmp");
        // Mode only applies at creation, so drop any stale temp file first.
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("removing {}", tmp.display()));
            }
            _ => {}
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(serde_json::to_string(self)?.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
    }

    pub fn insert(&mut self, name: &str) {
        self.names.insert(name.to_string());
    }

    pub fn remove(&mut self, name: &str) {
        self.names.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    #[test]
    fn save_resets_stale_temp_file_mode() {
        let dir = std::env::temp_dir().join(format!("cbox-idx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("index.json");
        let tmp = path.with_extension("json.tmp");
        std::fs::OpenOptions::new().write(true).create(true).mode(0o644).open(&tmp).unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut idx = Index::default();
        idx.insert("a");
        idx.save(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"names":["a"]}"#);
        std::fs::remove_dir_all(&dir).ok();
    }
}
