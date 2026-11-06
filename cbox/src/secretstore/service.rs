use super::{Index, SecretStore, StoredSecret, check_size, validate_name};
use anyhow::{Result, bail};
use std::path::Path;

pub struct ListRow {
    pub name: String,
    /// `None` = the index has the name but the store entry is gone.
    pub hosts: Option<Vec<String>>,
    pub env: Option<String>,
}

pub fn create(store: &dyn SecretStore, index_path: &Path, name: &str, s: StoredSecret) -> Result<()> {
    validate_name(name)?;
    let mut index = Index::load(index_path)?;
    if store.get(name)?.is_some() {
        if index.names.contains(name) {
            bail!("secret {name:?} already exists; run `cbox secret edit {name}` to change it");
        }
        bail!("secret {name:?} already exists in {} but is not in the index; run `cbox secret delete {name}` first", store.label());
    }
    let json = s.to_json();
    check_size(&json, store.max_entry_bytes())?;
    store.put(name, &s)?;
    index.insert(name);
    index.save(index_path)
}

pub fn list(store: &dyn SecretStore, index_path: &Path) -> Result<Vec<ListRow>> {
    Index::load(index_path)?
        .names
        .into_iter()
        .map(|name| {
            let s = store.get(&name)?;
            Ok(ListRow {
                hosts: s.as_ref().map(|s| s.hosts.clone()),
                env: s.map(|s| s.env),
                name,
            })
        })
        .collect()
}

pub fn delete(store: &dyn SecretStore, index_path: &Path, name: &str) -> Result<()> {
    store.delete(name)?;
    let mut index = Index::load(index_path)?;
    index.remove(name);
    index.save(index_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secretstore::KeyringStore;
    use std::os::unix::fs::PermissionsExt;

    fn setup(limit: Option<usize>) -> (KeyringStore, tempdir::D) {
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        (KeyringStore::new("mock", limit), tempdir::D::new())
    }

    // Minimal temp dir so we add no dev-dependency.
    mod tempdir {
        pub struct D(pub std::path::PathBuf);
        impl D {
            pub fn new() -> D {
                let p = std::env::temp_dir().join(format!("cbox-idx-{}-{:?}", std::process::id(), std::thread::current().id()).replace(['(', ')'], ""));
                let _ = std::fs::remove_dir_all(&p);
                D(p)
            }
            pub fn index(&self) -> std::path::PathBuf {
                self.0.join("cbox/secrets.json")
            }
        }
        impl Drop for D {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn sec() -> StoredSecret {
        StoredSecret { value: "sk-TOPSECRET".into(), hosts: vec!["api.openai.com".into()], env: "OPENAI_API_KEY".into() }
    }

    #[test]
    fn create_then_list_shows_hosts_and_env() {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(None);
        create(&st, &d.index(), "openai", sec()).unwrap();
        let rows = list(&st, &d.index()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "openai");
        assert_eq!(rows[0].hosts.as_deref(), Some(&["api.openai.com".to_string()][..]));
        assert_eq!(rows[0].env.as_deref(), Some("OPENAI_API_KEY"));
    }

    #[test]
    fn create_twice_errors_and_says_edit() {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(None);
        create(&st, &d.index(), "openai", sec()).unwrap();
        let e = create(&st, &d.index(), "openai", sec()).unwrap_err().to_string();
        assert!(e.contains("cbox secret edit"), "{e}");
        assert!(!e.contains("TOPSECRET"));
    }

    #[test]
    fn create_over_an_orphan_store_entry_says_delete() {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(None);
        st.put("openai", &sec()).unwrap();
        let e = create(&st, &d.index(), "openai", sec()).unwrap_err().to_string();
        assert!(e.contains("cbox secret delete"), "{e}");
    }

    #[test]
    fn create_rejects_oversize_before_writing() {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(Some(100));
        let mut s = sec();
        s.value = "x".repeat(200);
        let e = create(&st, &d.index(), "openai", s).unwrap_err().to_string();
        assert!(e.contains("limit"), "{e}");
        assert!(st.get("openai").unwrap().is_none());
        assert!(!d.index().exists());
    }

    #[test]
    fn list_marks_a_name_whose_entry_vanished_as_missing() {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(None);
        create(&st, &d.index(), "openai", sec()).unwrap();
        st.delete("openai").unwrap();
        let rows = list(&st, &d.index()).unwrap();
        assert!(rows[0].hosts.is_none() && rows[0].env.is_none());
    }

    #[test]
    fn delete_succeeds_when_entry_or_index_line_is_already_gone() {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(None);
        create(&st, &d.index(), "openai", sec()).unwrap();
        st.delete("openai").unwrap(); // entry gone
        delete(&st, &d.index(), "openai").unwrap();
        assert!(list(&st, &d.index()).unwrap().is_empty());
        delete(&st, &d.index(), "openai").unwrap(); // both gone
        create(&st, &d.index(), "a", sec()).unwrap();
        let mut i = Index::load(&d.index()).unwrap();
        i.remove("a");
        i.save(&d.index()).unwrap(); // index line gone
        delete(&st, &d.index(), "a").unwrap();
        assert!(st.get("a").unwrap().is_none());
    }

    #[test]
    fn index_file_is_0600_in_a_0700_dir() {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(None);
        create(&st, &d.index(), "openai", sec()).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&d.index()), 0o600);
        assert_eq!(mode(d.index().parent().unwrap()), 0o700);
    }

    #[test]
    fn index_contains_names_only() {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(None);
        create(&st, &d.index(), "openai", sec()).unwrap();
        let txt = std::fs::read_to_string(d.index()).unwrap();
        assert_eq!(txt, r#"{"names":["openai"]}"#);
        assert!(!txt.contains("TOPSECRET") && !txt.contains("api.openai.com"));
    }
}
