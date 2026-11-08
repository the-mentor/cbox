//! Named bundles of `up` choices, stored in `profiles.toml` (see docs/design/profiles.md).


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

#[derive(Debug, PartialEq)]
pub struct Profiles(pub BTreeMap<String, Profile>);

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

/// Write `body` to a temp file next to `path` (unique per process, 0600, dir 0700).
fn write_tmp(path: &Path, body: &str) -> Result<std::path::PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = path.parent().context("profiles path has no parent")?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
    // Mode only applies at creation, so drop any stale temp file from a reused pid first.
    std::fs::remove_file(&tmp).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }).with_context(|| format!("removing {}", tmp.display()))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    if let Err(e) = f.write_all(body.as_bytes()) {
        std::fs::remove_file(&tmp).ok();
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    Ok(tmp)
}

fn write_atomic(path: &Path, body: &str) -> Result<()> {
    let tmp = write_tmp(path, body)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        std::fs::remove_file(&tmp).ok();
        anyhow::anyhow!("renaming {} to {}: {e}", tmp.display(), path.display())
    })
}

/// Create the file with an empty `[default]` unless the path exists. Returns whether it did.
pub fn ensure_file(path: &Path) -> Result<bool> {
    create_new_with(path, "[default]\n")
}

/// Create `path` with `body` only if absent. `hard_link` fails if the target exists
/// (rename would replace it), so a file that appears at any moment is never touched.
fn create_new_with(path: &Path, body: &str) -> Result<bool> {
    create_new_via(path, body, |tmp, to| std::fs::hard_link(tmp, to))
}

/// `link` stands in for `hard_link` so tests can force the fallback.
fn create_new_via(path: &Path, body: &str, link: impl Fn(&Path, &Path) -> std::io::Result<()>) -> Result<bool> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = write_tmp(path, body)?;
    let r = link(&tmp, path);
    std::fs::remove_file(&tmp).ok();
    match r {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        // Filesystems without hard links (FAT, some SMB/9p/FUSE mounts): create_new still never replaces.
        // A crash mid-write can leave a partial file on this fallback path only.
        Err(_) => match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
            Ok(mut f) => f.write_all(body.as_bytes()).map(|()| true).with_context(|| format!("writing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e).with_context(|| format!("creating {}", path.display())),
        },
    }
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
    // One field at a time so every bad value is reported; name the field, not the CLI flag.
    for r in [
        crate::boxopts::check_resources(p.disk_size, None, None),
        crate::boxopts::check_resources(None, p.memory, None),
        crate::boxopts::check_resources(None, None, p.cpus),
    ] {
        if let Err(e) = r {
            bad(e.to_string().trim_start_matches("--").replace("disk-size", "disk_size"));
        }
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
        assert!(v.iter().any(|m| m.contains("invalid profile name")));
        assert!(v.iter().any(|m| m.contains("secrets:") && m.contains("gh")));
        assert!(v.iter().all(|m| m.starts_with("profile Bad:")), "{v:?}");
        assert!(v.iter().any(|m| m.contains("name")));
        assert!(v.iter().any(|m| m.contains("gh")));
        assert!(v.iter().any(|m| m.contains("duplicate")));
        assert!(v.iter().any(|m| m.contains("no secret named nope")));
        assert!(v.iter().any(|m| m.contains("memory must be greater than 0") && !m.contains("--")));
        let e = Profile { image: Some(String::new()), ..Default::default() };
        assert!(validate("ok", &e, &known)[0].contains("image"));
    }

    #[test]
    fn validate_reports_each_bad_resource_by_field() {
        let p = Profile { disk_size: Some(0), memory: Some(0), cpus: Some(0), ..Default::default() };
        let v = validate("x", &p, &BTreeSet::new());
        assert_eq!(v, ["profile x: disk_size must be greater than 0", "profile x: memory must be greater than 0", "profile x: cpus must be greater than 0"]);
    }

    #[test]
    fn create_new_with_leaves_an_existing_file_untouched() {
        let f = tmp("race").join("profiles.toml");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "[real]\n").unwrap();
        assert!(!create_new_with(&f, "[default]\n").unwrap());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "[real]\n");
        assert_eq!(std::fs::read_dir(f.parent().unwrap()).unwrap().count(), 1, "tmp file left behind");
    }

    #[test]
    fn create_new_falls_back_when_linking_fails() {
        use std::os::unix::fs::PermissionsExt;
        let no_link = |_: &Path, _: &Path| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let f = tmp("fallback").join("profiles.toml");
        assert!(create_new_via(&f, "[default]\n", no_link).unwrap());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "[default]\n");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::write(&f, "[real]\n").unwrap();
        assert!(!create_new_via(&f, "[default]\n", no_link).unwrap());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "[real]\n");
        assert_eq!(std::fs::read_dir(f.parent().unwrap()).unwrap().count(), 1, "tmp file left behind");
    }

    #[test]
    fn ensure_file_creates_0600_in_a_0700_dir() {
        use std::os::unix::fs::PermissionsExt;
        let f = tmp("perm").join("d/profiles.toml");
        assert!(ensure_file(&f).unwrap());
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(f.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn validate_accepts_a_clean_profile() {
        let known: BTreeSet<String> = ["good".to_string()].into();
        let p = Profile { secrets: vec!["good".into()], image: Some("i".into()), cpus: Some(2), memory: Some(4), disk_size: Some(10) };
        assert!(validate("client-a", &p, &known).is_empty());
    }
}
