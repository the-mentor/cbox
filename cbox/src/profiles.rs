//! Named bundles of `up` choices, stored in `profiles.toml` (see docs/design/profiles.md).

// Consumed by the `profile` and `up` commands, added in the following tasks.
#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const DEFAULT: &str = "default";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_size: Option<u64>,
}

pub struct Profiles(pub BTreeMap<String, Profile>);

// Hand-written so tests can compare; the map is the whole state.
impl PartialEq for Profiles {
    fn eq(&self, o: &Self) -> bool {
        self.0 == o.0
    }
}
impl std::fmt::Debug for Profiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Profiles {
    pub fn load(path: &Path) -> Result<Profiles> {
        let mut map: BTreeMap<String, Profile> = match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|e| anyhow::anyhow!("invalid {}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        map.entry(DEFAULT.to_string()).or_default();
        Ok(Profiles(map))
    }

    /// Atomic: temp file in the same directory, then rename.
    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, &toml::to_string_pretty(&self.0)?)
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.0.get(name)
    }
}

fn write_atomic(path: &Path, body: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = path.parent().context("profiles path has no parent")?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("toml.tmp");
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
    f.write_all(body.as_bytes()).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}

/// Create the file with an empty `[default]` unless the path exists. Returns whether it did.
pub fn ensure_file(path: &Path) -> Result<bool> {
    // shortcut: exists-then-write is racy with another cbox starting at once; the loser's rename just rewrites the same bytes.
    if path.try_exists().with_context(|| format!("checking {}", path.display()))? {
        return Ok(false);
    }
    write_atomic(path, "[default]\n")?;
    Ok(true)
}

/// One message per problem, each prefixed `profile NAME:`.
pub fn validate(name: &str, p: &Profile, known_secrets: &BTreeSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut bad = |m: String| out.push(format!("profile {name}: {m}"));
    if !crate::secretstore::name_charset_ok(name) {
        bad("invalid profile name: use lowercase letters, digits, '-' or '_', starting with a letter".into());
    }
    let mut seen = BTreeSet::new();
    for s in &p.secrets {
        if let Err(e) = crate::secretstore::validate_name(s) {
            bad(format!("secrets: {e}"));
        } else if !known_secrets.contains(s) {
            bad(format!("secrets: no secret named {s}"));
        }
        if !seen.insert(s) {
            bad(format!("secrets: duplicate secret {s}"));
        }
    }
    if let Err(e) = crate::boxopts::check_resources(p.disk_size, p.memory, p.cpus) {
        bad(e.to_string());
    }
    if p.image.as_deref() == Some("") {
        bad("image must not be empty".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cbox-prof-{}-{tag}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        d
    }

    #[test]
    fn load_of_a_missing_file_has_an_empty_default() {
        let p = Profiles::load(&tmp("missing").join("profiles.toml")).unwrap();
        assert_eq!(p.get(DEFAULT), Some(&Profile::default()));
        assert_eq!(p.0.len(), 1);
    }

    #[test]
    fn load_rejects_an_unknown_key_naming_the_path() {
        let d = tmp("unk");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("profiles.toml");
        std::fs::write(&f, "[default]\nmemroy = 4\n").unwrap();
        let e = Profiles::load(&f).unwrap_err().to_string();
        assert!(e.contains(&f.display().to_string()) && e.contains("memroy"), "{e}");
    }

    #[test]
    fn load_inserts_default_when_the_file_lacks_it() {
        let d = tmp("nodef");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("profiles.toml");
        std::fs::write(&f, "[x]\nmemory = 2\n").unwrap();
        let p = Profiles::load(&f).unwrap();
        assert_eq!(p.get(DEFAULT), Some(&Profile::default()));
        assert_eq!(p.get("x").unwrap().memory, Some(2));
    }

    #[test]
    fn save_then_load_round_trips_and_writes_0600() {
        use std::os::unix::fs::PermissionsExt;
        let f = tmp("rt").join("sub/profiles.toml");
        let mut p = Profiles::load(&f).unwrap();
        p.0.insert("a".into(), Profile { secrets: vec!["s1".into(), "s0".into()], image: Some("img".into()), cpus: Some(3), memory: Some(8), disk_size: Some(20) });
        p.save(&f).unwrap();
        assert_eq!(Profiles::load(&f).unwrap(), p);
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(f.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(!std::fs::read_to_string(&f).unwrap().contains("cpus = null"));
    }

    #[test]
    fn ensure_file_creates_once_and_never_overwrites() {
        let f = tmp("ens").join("profiles.toml");
        assert!(ensure_file(&f).unwrap());
        assert!(!ensure_file(&f).unwrap());
        assert!(Profiles::load(&f).unwrap().get(DEFAULT).is_some());
        std::fs::write(&f, "[x]\n").unwrap();
        assert!(!ensure_file(&f).unwrap());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "[x]\n");
    }

    #[test]
    fn validate_reports_every_problem() {
        let known: BTreeSet<String> = ["good".to_string()].into();
        let p = Profile { secrets: vec!["gh".into(), "good".into(), "good".into(), "nope".into()], memory: Some(0), ..Default::default() };
        let v = validate("Bad", &p, &known);
        assert_eq!(v.len(), 5, "{v:?}");
        assert!(v.iter().all(|m| m.starts_with("profile Bad:")), "{v:?}");
        assert!(v.iter().any(|m| m.contains("name")));
        assert!(v.iter().any(|m| m.contains("gh")));
        assert!(v.iter().any(|m| m.contains("duplicate")));
        assert!(v.iter().any(|m| m.contains("no secret named nope")));
        assert!(v.iter().any(|m| m.contains("--memory must be greater than 0")));
        let e = Profile { image: Some(String::new()), ..Default::default() };
        assert!(validate("ok", &e, &known)[0].contains("image"));
    }

    #[test]
    fn validate_accepts_a_clean_profile() {
        let known: BTreeSet<String> = ["good".to_string()].into();
        let p = Profile { secrets: vec!["good".into()], image: Some("i".into()), cpus: Some(2), memory: Some(4), disk_size: Some(10) };
        assert!(validate("client-a", &p, &known).is_empty());
    }
}
