use super::{Index, SecretStore, StoredSecret, check_size, validate_env, validate_name};
use anyhow::{Result, anyhow, bail};
use std::path::Path;

pub struct ListRow {
    pub name: String,
    /// `None` = the index has the name but the store entry is gone.
    pub hosts: Option<Vec<String>>,
    pub env: Option<String>,
}

/// Fails if `name` is already stored; callers run it before asking for a value.
pub fn check_absent(store: &dyn SecretStore, index_path: &Path, name: &str) -> Result<()> {
    validate_name(name)?;
    if store.exists(name)? {
        if Index::load(index_path)?.names.contains(name) {
            bail!("secret {name:?} already exists; run `cbox secret edit {name}` to change it");
        }
        bail!("secret {name:?} already exists in {} but is not in the index; run `cbox secret delete {name}` first", store.label());
    }
    Ok(())
}

/// True if the store entry or the index line exists.
pub fn exists(store: &dyn SecretStore, index_path: &Path, name: &str) -> Result<bool> {
    validate_name(name)?;
    Ok(store.exists(name)? || Index::load(index_path)?.names.contains(name))
}

pub fn create(store: &dyn SecretStore, index_path: &Path, name: &str, s: StoredSecret) -> Result<()> {
    check_absent(store, index_path, name)?;
    let mut index = Index::load(index_path)?;
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

/// Applied in order: replace, then add (deduped), then remove.
#[derive(Default)]
pub struct HostChange {
    pub replace: Option<Vec<String>>,
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

pub struct EditChanges {
    pub value: Option<String>,
    pub hosts: HostChange,
    pub env: Option<String>,
}

#[derive(Debug)]
pub struct EditOutcome {
    pub old_hosts: Vec<String>,
    pub new_hosts: Vec<String>,
}

fn add_hosts(hosts: &mut Vec<String>, add: Vec<String>) {
    for h in add {
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    }
}

/// Read-modify-write of one entry; nothing is written unless every check passes.
pub fn edit(store: &dyn SecretStore, name: &str, c: EditChanges) -> Result<EditOutcome> {
    validate_name(name)?;
    let mut s = store.get(name)?.ok_or_else(|| anyhow!("secret {name:?} not found; run cbox secret create {name}"))?;
    let old_hosts = s.hosts.clone();
    if let Some(h) = c.hosts.replace {
        s.hosts = h;
    }
    add_hosts(&mut s.hosts, c.hosts.add);
    s.hosts.retain(|x| !c.hosts.remove.contains(x));
    if s.hosts.is_empty() {
        bail!("at least one host is required; secret {name:?} was not changed");
    }
    if let Some(e) = c.env {
        validate_env(&e)?;
        s.env = e;
    }
    if let Some(v) = c.value {
        s.value = v;
    }
    check_size(&s.to_json(), store.max_entry_bytes())?;
    store.put(name, &s)?;
    Ok(EditOutcome { old_hosts, new_hosts: s.hosts })
}

pub fn delete(store: &dyn SecretStore, index_path: &Path, name: &str) -> Result<()> {
    validate_name(name)?;
    let had_entry = store.delete(name)?;
    let mut index = Index::load(index_path)?;
    if !had_entry && !index.names.contains(name) {
        bail!("secret {name:?} not found");
    }
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
        create(&st, &d.index(), "a", sec()).unwrap();
        let mut i = Index::load(&d.index()).unwrap();
        i.remove("a");
        i.save(&d.index()).unwrap(); // index line gone
        delete(&st, &d.index(), "a").unwrap();
        assert!(st.get("a").unwrap().is_none());
    }

    #[test]
    fn delete_of_a_name_in_neither_place_fails_as_not_found() {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(None);
        create(&st, &d.index(), "keep", sec()).unwrap();
        let e = delete(&st, &d.index(), "nope").unwrap_err().to_string();
        assert_eq!(e, "secret \"nope\" not found");
        assert!(!exists(&st, &d.index(), "nope").unwrap());
        assert!(exists(&st, &d.index(), "keep").unwrap());
    }

    fn corrupt_delete_case(raw: &str, indexed: bool) {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(None);
        keyring_core::Entry::new("cbox", "bad").unwrap().set_password(raw).unwrap();
        if indexed {
            let mut i = Index::load(&d.index()).unwrap();
            i.insert("bad");
            i.save(&d.index()).unwrap();
        }
        assert!(exists(&st, &d.index(), "bad").unwrap());
        delete(&st, &d.index(), "bad").unwrap();
        assert!(matches!(keyring_core::Entry::new("cbox", "bad").unwrap().get_password(), Err(keyring_core::Error::NoEntry)));
        assert!(!Index::load(&d.index()).unwrap().names.contains("bad"));
        assert!(!exists(&st, &d.index(), "bad").unwrap());
    }

    #[test]
    fn corrupt_entries_are_present_and_deletable() {
        for raw in ["CORRUPT-TEXT{", r#"{"value":"x","hosts":"sk-LEAKME","env":"E"}"#] {
            corrupt_delete_case(raw, true);
            corrupt_delete_case(raw, false); // not in the index
        }
    }

    #[test]
    fn create_over_a_corrupt_entry_refuses_without_leaking() {
        let _l = crate::test_env_lock::lock();
        for raw in ["CORRUPT-TEXT{", r#"{"value":"x","hosts":"sk-LEAKME","env":"E"}"#] {
            for in_index in [true, false] {
                let (st, d) = setup(None);
                let entry = keyring_core::Entry::new("cbox", "bad").unwrap();
                entry.set_password(raw).unwrap();
                if in_index {
                    let mut i = Index::load(&d.index()).unwrap();
                    i.insert("bad");
                    i.save(&d.index()).unwrap();
                }
                let before = std::fs::read(d.index()).ok();
                let e = format!("{:#}", create(&st, &d.index(), "bad", sec()).unwrap_err());
                let hint = if in_index { "cbox secret edit" } else { "cbox secret delete" };
                assert!(e.contains("already exists") && e.contains(hint), "{e}");
                assert!(!e.contains("LEAKME") && !e.contains("invalid stored secret JSON"), "{e}");
                assert_eq!(entry.get_password().unwrap(), raw);
                assert_eq!(std::fs::read(d.index()).ok(), before);
            }
        }
    }

    #[test]
    fn check_absent_fails_for_existing_names_before_any_write() {
        let _l = crate::test_env_lock::lock();
        let (st, d) = setup(None);
        check_absent(&st, &d.index(), "openai").unwrap();
        create(&st, &d.index(), "openai", sec()).unwrap();
        let e = check_absent(&st, &d.index(), "openai").unwrap_err().to_string();
        assert!(e.contains("already exists") && e.contains("cbox secret edit"), "{e}");
    }

    #[test]
    fn corrupt_entry_error_names_the_secret_and_no_content() {
        let _l = crate::test_env_lock::lock();
        let (st, _d) = setup(None);
        keyring_core::Entry::new("cbox", "bad").unwrap().set_password("CORRUPT-TEXT{").unwrap();
        let e = format!("{:#}", st.get("bad").unwrap_err());
        assert!(e.contains("\"bad\"") && e.contains("mock"), "{e}");
        assert!(!e.contains("CORRUPT"), "{e}");
    }

    #[test]
    fn add_remove_adds_then_removes() {
        let _l = crate::test_env_lock::lock();
        let (st, _d) = setup(None);
        put_sec(&st, &["a.com"]);
        edit(&st, "k", ch(HostChange { add: v(&["b.com"]), remove: v(&["a.com"]), ..Default::default() })).unwrap();
        assert_eq!(st.get("k").unwrap().unwrap().hosts, v(&["b.com"]));
        // Adding and removing the same host empties the list: error, nothing written.
        let e = edit(&st, "k", ch(HostChange { add: v(&["b.com"]), remove: v(&["b.com"]), ..Default::default() })).unwrap_err().to_string();
        assert!(e.contains("at least one host"), "{e}");
        assert_eq!(st.get("k").unwrap().unwrap().hosts, v(&["b.com"]));
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

    fn put_sec(st: &KeyringStore, hosts: &[&str]) {
        st.put("k", &StoredSecret { value: "v1".into(), hosts: hosts.iter().map(|s| s.to_string()).collect(), env: "E".into() }).unwrap();
    }
    fn ch(hosts: HostChange) -> EditChanges {
        EditChanges { value: None, hosts, env: None }
    }
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn host_replaces_the_whole_list() {
        let _l = crate::test_env_lock::lock();
        let (st, _d) = setup(None);
        put_sec(&st, &["a.com", "b.com"]);
        let o = edit(&st, "k", ch(HostChange { replace: Some(v(&["c.com"])), ..Default::default() })).unwrap();
        assert_eq!((o.old_hosts, o.new_hosts), (v(&["a.com", "b.com"]), v(&["c.com"])));
        assert_eq!(st.get("k").unwrap().unwrap().hosts, v(&["c.com"]));
    }

    #[test]
    fn add_host_dedupes() {
        let _l = crate::test_env_lock::lock();
        let (st, _d) = setup(None);
        put_sec(&st, &["a.com"]);
        edit(&st, "k", ch(HostChange { add: v(&["a.com", "b.com"]), ..Default::default() })).unwrap();
        assert_eq!(st.get("k").unwrap().unwrap().hosts, v(&["a.com", "b.com"]));
    }

    #[test]
    fn remove_host_removes_only_that_host() {
        let _l = crate::test_env_lock::lock();
        let (st, _d) = setup(None);
        put_sec(&st, &["a.com", "b.com"]);
        edit(&st, "k", ch(HostChange { remove: v(&["a.com", "zzz.com"]), ..Default::default() })).unwrap();
        assert_eq!(st.get("k").unwrap().unwrap().hosts, v(&["b.com"]));
    }

    #[test]
    fn edit_refuses_to_empty_the_host_list() {
        let _l = crate::test_env_lock::lock();
        let (st, _d) = setup(None);
        put_sec(&st, &["a.com"]);
        for c in [HostChange { remove: v(&["a.com"]), ..Default::default() }, HostChange { replace: Some(vec![]), ..Default::default() }] {
            let e = edit(&st, "k", ch(c)).unwrap_err().to_string();
            assert!(e.contains("at least one host"), "{e}");
        }
        assert_eq!(st.get("k").unwrap().unwrap().hosts, v(&["a.com"]));
    }

    #[test]
    fn edit_replaces_value_and_env() {
        let _l = crate::test_env_lock::lock();
        let (st, _d) = setup(None);
        put_sec(&st, &["a.com"]);
        edit(&st, "k", EditChanges { value: Some("v2".into()), hosts: HostChange::default(), env: Some("NEW".into()) }).unwrap();
        let s = st.get("k").unwrap().unwrap();
        assert_eq!((s.value.as_str(), s.env.as_str()), ("v2", "NEW"));
        assert!(edit(&st, "k", EditChanges { value: None, hosts: HostChange::default(), env: Some("1x".into()) }).is_err());
    }

    #[test]
    fn edit_checks_size_and_writes_nothing_when_over_limit() {
        let _l = crate::test_env_lock::lock();
        let (st, _d) = setup(Some(100));
        put_sec(&st, &["a.com"]);
        let e = edit(&st, "k", EditChanges { value: Some("x".repeat(200)), hosts: HostChange::default(), env: None }).unwrap_err().to_string();
        assert!(e.contains("limit") && !e.contains("xxxx"), "{e}");
        assert_eq!(st.get("k").unwrap().unwrap().value, "v1");
    }

    #[test]
    fn edit_missing_secret_says_create() {
        let _l = crate::test_env_lock::lock();
        let (st, _d) = setup(None);
        let e = edit(&st, "x", ch(HostChange::default())).unwrap_err().to_string();
        assert_eq!(e, "secret \"x\" not found; run cbox secret create x");
    }
}
