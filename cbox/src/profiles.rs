//! Named bundles of `up` choices, stored in `profiles.toml` (see docs/design/profiles.md).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::sidecar::ProfileSnapshot;

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
            Ok(s) => toml::from_str(&s).map_err(|e| parse_error(path, &s, &e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        map.entry(DEFAULT.to_string()).or_default();
        Ok(Profiles(map))
    }

    /// Atomic: temp file in the same directory, then rename.
    pub fn save(&self, path: &Path) -> Result<()> {
        // The toml error text is never shown: it can quote a value.
        let body = toml::to_string_pretty(&self.0).map_err(|_| anyhow::anyhow!("cannot encode profiles for {}", path.display()))?;
        write_atomic(path, &body)
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.0.get(name)
    }
}

/// Path and location only: toml's message and source excerpt can quote a pasted value.
fn parse_error(path: &Path, src: &str, e: &toml::de::Error) -> anyhow::Error {
    let at = e.span().map_or(String::new(), |s| {
        let before = src.get(..s.start).unwrap_or(src);
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        format!("line {line}, column {column}: ")
    });
    anyhow::anyhow!("invalid {}: {at}not valid TOML or not a valid profile (unknown key or wrong type); fix or remove that entry", path.display())
}

/// Sorted names of the profiles that list `secret`.
pub fn profiles_using(profiles: &Profiles, secret: &str) -> Vec<String> {
    profiles.0.iter().filter(|(_, p)| p.secrets.iter().any(|s| s == secret)).map(|(n, _)| n.clone()).collect()
}

/// A profile with the flags of one `up` applied. `None` resources mean the built-in default.
#[derive(Debug)]
pub struct Resolved {
    pub profile_name: String,
    /// Profile secrets in file order, then flag secrets as typed.
    pub secrets: Vec<String>,
    pub image: String,
    pub cpus: Option<u8>,
    pub memory_gb: Option<u32>,
    pub disk_size_gb: Option<u64>,
}

impl Resolved {
    pub fn snapshot(&self) -> ProfileSnapshot {
        ProfileSnapshot {
            name: self.profile_name.clone(),
            // Names only: no env var, host or pasted value from a `NAME=ENV@hosts` flag.
            secrets: self.secrets.iter().map(|s| s.split('=').next().unwrap_or(s).to_string()).collect(),
            image: self.image.clone(),
            cpus: self.cpus,
            memory_gb: self.memory_gb,
            disk_size_gb: self.disk_size_gb,
        }
    }
}

pub fn resolve(
    profiles: &Profiles,
    name: &str,
    flag_secrets: &[String],
    image: Option<&str>,
    cpus: Option<u8>,
    memory_gb: Option<u32>,
    disk_size_gb: Option<u64>,
) -> Result<Resolved> {
    let p = profiles.get(name).with_context(|| format!("no profile named {name}"))?;
    for f in flag_secrets {
        let flag_name = f.split('=').next().unwrap_or(f);
        if p.secrets.iter().any(|s| s == flag_name) {
            anyhow::bail!("secret {flag_name} is already in profile {name}");
        }
    }
    Ok(Resolved {
        profile_name: name.to_string(),
        secrets: p.secrets.iter().chain(flag_secrets).cloned().collect(),
        image: image.map(String::from).or_else(|| p.image.clone()).unwrap_or_else(|| "cbox-custom".into()),
        cpus: cpus.or(p.cpus),
        memory_gb: memory_gb.or(p.memory),
        disk_size_gb: disk_size_gb.or(p.disk_size),
    })
}

/// Without a resolvable config dir there is no file: an empty `default`, like a missing file.
pub fn load_or_empty(path: Option<&Path>) -> Result<Profiles> {
    match path {
        Some(p) => Profiles::load(p),
        None => Ok(Profiles(BTreeMap::from([(DEFAULT.to_string(), Profile::default())]))),
    }
}

/// The names-only secrets index, read only when the profile names secrets.
pub fn known_for_up(p: &Profile, known: impl FnOnce() -> Result<BTreeSet<String>>) -> Result<BTreeSet<String>> {
    if p.secrets.is_empty() { Ok(BTreeSet::new()) } else { known() }
}

/// What `up` does with a profile's validation problems.
#[derive(Debug, Default, PartialEq)]
pub struct UpCheck {
    /// Set when problems were downgraded to warnings: the error a new box gets instead.
    pub downgraded: Option<String>,
    pub warnings: Vec<String>,
    /// Profile secrets this run leaves out: unknown to the index or not a usable name.
    pub drop_secrets: Vec<String>,
}

/// A new box (or `-f`) fails on any problem. A resumed box keeps its creation settings, so
/// it only warns and leaves out the profile secrets that cannot be read.
pub fn up_check(problems: Vec<String>, resuming: bool, profile_secrets: &[String], known: &BTreeSet<String>, file: &str) -> Result<UpCheck> {
    if problems.is_empty() {
        return Ok(UpCheck::default());
    }
    let error = format!("{} ({file})", problems.join("\n"));
    if !resuming {
        anyhow::bail!("{error}");
    }
    let mut warnings: Vec<String> = problems.iter().map(|p| format!("cbox: warning: {p} ({file})")).collect();
    warnings.push(
        "cbox: warning: resuming anyway: the box keeps the settings it was created with, and this run ignores \
         the missing secrets. Fix the profile, then run with -f/--force to recreate the box."
            .into(),
    );
    let drop_secrets = profile_secrets
        .iter()
        .filter(|s| crate::secretstore::validate_name(s).is_err() || !known.contains(*s))
        .cloned()
        .collect();
    Ok(UpCheck { downgraded: Some(error), warnings, drop_secrets })
}

impl Resolved {
    /// Remove `drop` (and repeats) from the first `n_profile` secrets, the profile's; flag secrets stay.
    pub fn drop_profile_secrets(&mut self, n_profile: usize, drop: &[String]) {
        let flags = self.secrets.split_off(n_profile.min(self.secrets.len()));
        let mut seen = BTreeSet::new();
        self.secrets.retain(|s| !drop.contains(s) && seen.insert(s.clone()));
        self.secrets.extend(flags);
    }
}

/// `--profile` if given, else the box's recorded profile, else `default`.
pub fn choose(asked: Option<&str>, recorded: Option<&str>, profiles: &Profiles) -> Result<String> {
    match (asked, recorded) {
        (Some(a), _) => Ok(a.to_string()),
        (None, Some(r)) if profiles.get(r).is_none() => anyhow::bail!(
            "profile {r} (used to create this box) no longer exists in profiles.toml; pass --profile NAME"
        ),
        (None, Some(r)) => Ok(r.to_string()),
        (None, None) => Ok(DEFAULT.to_string()),
    }
}

/// Human-readable differences, empty when the snapshots match.
pub fn diff(old: &ProfileSnapshot, new: &ProfileSnapshot) -> Vec<String> {
    let mut out = Vec::new();
    let list = |a: &[String], b: &[String]| a.iter().filter(|s| !b.contains(s)).cloned().collect::<Vec<_>>().join(", ");
    let (added, removed) = (list(&new.secrets, &old.secrets), list(&old.secrets, &new.secrets));
    if !added.is_empty() {
        out.push(format!("secrets added: {added}"));
    }
    if !removed.is_empty() {
        out.push(format!("secrets removed: {removed}"));
    }
    if old.image != new.image {
        out.push(format!("image: {} -> {}", old.image, new.image));
    }
    fn num<T: std::fmt::Display + PartialEq>(out: &mut Vec<String>, label: &str, unit: &str, a: Option<T>, b: Option<T>) {
        if a != b {
            let show = |v: &Option<T>| v.as_ref().map_or("default".to_string(), |v| v.to_string());
            let unit = if b.is_some() { unit } else { "" };
            out.push(format!("{label}: {} -> {}{unit}", show(&a), show(&b)));
        }
    }
    num(&mut out, "cpus", "", old.cpus, new.cpus);
    num(&mut out, "memory", " GiB", old.memory_gb, new.memory_gb);
    num(&mut out, "disk size", " GB", old.disk_size_gb, new.disk_size_gb);
    out
}

/// The stderr warning for a resumed box whose profile now resolves differently.
/// `asked` is the `--profile` passed on this run, if any.
pub fn resume_warning(old: Option<&ProfileSnapshot>, new: &ProfileSnapshot, asked: Option<&str>) -> Option<String> {
    let old = old?; // boxes from before profiles record none: stay quiet
    let lines = diff(old, new);
    let other = asked.filter(|x| *x != old.name);
    if lines.is_empty() && other.is_none() {
        return None;
    }
    let head = match other {
        Some(x) => {
            format!("you asked for profile {x}; this box was created with profile {}", old.name)
        }
        None => format!("profile {} differs from this box (created with {})", new.name, old.name),
    };
    let body: String = lines.iter().map(|l| format!("  {l}\n")).collect();
    Some(format!(
        "cbox: warning: {head}:\n{body}the box keeps its OLD settings until you run with -f/--force to recreate it."
    ))
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
    // Already there (even a dangling symlink): touch nothing, so a read-only config dir stays quiet.
    if std::fs::symlink_metadata(path).is_ok() {
        return Ok(false);
    }
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
        // A failed write removes the file it just created; only a crash mid-write can leave a partial one.
        Err(orig) => match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
            Ok(mut f) => match f.write_all(body.as_bytes()) {
                Ok(()) => Ok(true),
                Err(e) => {
                    std::fs::remove_file(path).ok();
                    Err(e).with_context(|| format!("writing {}", path.display()))
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e).with_context(|| format!("creating {} (hard link failed first: {orig})", path.display())),
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

    #[test]
    fn profiles_using_lists_sorted_names() {
        let mut p = two();
        p.0.insert("aaa".into(), Profile { secrets: vec!["b".into()], ..Default::default() });
        assert_eq!(profiles_using(&p, "b"), ["aaa", "foo"]);
        assert!(profiles_using(&p, "zzz").is_empty());
    }

    fn two() -> Profiles {
        let mut m = BTreeMap::new();
        m.insert("default".into(), Profile { memory: Some(8), ..Default::default() });
        m.insert("foo".into(), Profile { secrets: vec!["a".into(), "b".into()], ..Default::default() });
        Profiles(m)
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolve_uses_only_the_chosen_profile_and_builtin_image() {
        let r = resolve(&two(), "default", &[], None, None, None, None).unwrap();
        assert_eq!((r.memory_gb, r.image.as_str()), (Some(8), "cbox-custom"));
        let r = resolve(&Profiles(BTreeMap::from([("foo".into(), Profile::default())])), "foo", &[], None, None, None, None).unwrap();
        assert_eq!(r.memory_gb, None);
    }

    #[test]
    fn resolve_merges_flag_secrets_after_profile_secrets() {
        let r = resolve(&two(), "foo", &strs(&["z", "Q=ENV@h"]), None, None, None, None).unwrap();
        assert_eq!(r.secrets, strs(&["a", "b", "z", "Q=ENV@h"]));
    }

    #[test]
    fn snapshot_keeps_secret_names_only() {
        let r = resolve(&two(), "foo", &strs(&["Q=ENV@h"]), None, None, None, None).unwrap();
        assert_eq!(r.snapshot().secrets, strs(&["a", "b", "Q"]));
    }

    #[test]
    fn same_contents_but_another_profile_asked_still_warns() {
        let a = resolve(&two(), "foo", &[], None, None, None, None).unwrap().snapshot();
        let mut b = a.clone();
        b.name = "bar".into();
        let w = resume_warning(Some(&a), &b, Some("bar")).unwrap();
        assert!(w.starts_with("cbox: warning: you asked for profile bar; this box was created with profile foo:\n"), "{w}");
        assert_eq!(resume_warning(Some(&a), &a, Some("foo")), None);
    }

    #[test]
    fn no_config_dir_means_an_empty_default_profile() {
        let p = load_or_empty(None).unwrap();
        assert_eq!(p.get(DEFAULT), Some(&Profile::default()));
    }

    #[test]
    fn the_secrets_index_is_not_read_for_a_profile_without_secrets() {
        let boom = || -> Result<BTreeSet<String>> { anyhow::bail!("corrupt index") };
        assert!(known_for_up(&Profile::default(), boom).unwrap().is_empty());
        let with = Profile { secrets: vec!["a".into()], ..Default::default() };
        assert!(known_for_up(&with, boom).is_err());
    }

    #[test]
    fn up_check_fails_a_new_box_and_warns_a_resumed_one() {
        let known: BTreeSet<String> = ["a".to_string()].into();
        let p = Profile { secrets: strs(&["a", "gone"]), ..Default::default() };
        let problems = validate("foo", &p, &known);
        let e = up_check(problems.clone(), false, &p.secrets, &known, "/x/profiles.toml").unwrap_err().to_string();
        assert_eq!(e, "profile foo: secrets: no secret named gone (/x/profiles.toml)");
        let c = up_check(problems, true, &p.secrets, &known, "/x/profiles.toml").unwrap();
        assert_eq!(c.downgraded.as_deref(), Some(e.as_str()));
        assert_eq!(c.drop_secrets, strs(&["gone"]));
        assert!(c.warnings[0].starts_with("cbox: warning: profile foo: secrets: no secret named gone"), "{:?}", c.warnings);
        assert!(c.warnings.last().unwrap().contains("-f/--force"), "{:?}", c.warnings);
        assert_eq!(up_check(vec![], false, &p.secrets, &known, "f").unwrap(), UpCheck::default());
    }

    #[test]
    fn up_check_on_resume_warns_for_a_resource_problem_and_drops_nothing() {
        let p = Profile { cpus: Some(0), ..Default::default() };
        let c = up_check(validate("foo", &p, &BTreeSet::new()), true, &p.secrets, &BTreeSet::new(), "f").unwrap();
        assert!(c.warnings[0].contains("cpus must be greater than 0"), "{:?}", c.warnings);
        assert!(c.drop_secrets.is_empty());
    }

    #[test]
    fn dropping_profile_secrets_never_drops_a_flag_secret() {
        let mut p = two();
        p.0.get_mut("foo").unwrap().secrets = strs(&["a", "gone", "a"]);
        let mut r = resolve(&p, "foo", &strs(&["gone2"]), None, None, None, None).unwrap();
        r.drop_profile_secrets(3, &strs(&["gone", "gone2"]));
        assert_eq!(r.secrets, strs(&["a", "gone2"]));
    }

    #[test]
    fn ensure_file_on_a_read_only_dir_with_an_existing_file_is_quiet() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("cbox-ro-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("profiles.toml");
        std::fs::write(&f, "[default]\n").unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o500)).unwrap();
        let r = ensure_file(&f);
        let names = std::fs::read_dir(&d).unwrap().count();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&d).ok();
        assert!(!r.unwrap());
        assert_eq!(names, 1);
    }

    #[test]
    fn choose_picks_asked_then_recorded_then_default() {
        let p = two();
        assert_eq!(choose(Some("default"), Some("foo"), &p).unwrap(), "default");
        assert_eq!(choose(None, Some("foo"), &p).unwrap(), "foo");
        assert_eq!(choose(None, None, &p).unwrap(), "default");
        let e = choose(None, Some("gone"), &p).unwrap_err().to_string();
        assert!(e.contains("gone") && e.contains("--profile"), "{e}");
        assert_eq!(choose(Some("other"), Some("gone"), &p).unwrap(), "other");
    }

    #[test]
    fn resolve_rejects_a_flag_secret_already_in_the_profile() {
        for f in ["b", "b=ENV@h"] {
            let e = resolve(&two(), "foo", &strs(&[f]), None, None, None, None).unwrap_err().to_string();
            assert_eq!(e, "secret b is already in profile foo");
        }
    }

    #[test]
    fn resolve_flags_override_single_values() {
        let r = resolve(&two(), "default", &[], Some("img"), Some(3), Some(16), Some(20)).unwrap();
        assert_eq!((r.image.as_str(), r.cpus, r.memory_gb, r.disk_size_gb), ("img", Some(3), Some(16), Some(20)));
    }

    #[test]
    fn resolve_unknown_profile_errors() {
        let e = resolve(&two(), "nope", &[], None, None, None, None).unwrap_err().to_string();
        assert_eq!(e, "no profile named nope");
    }

    #[test]
    fn diff_reports_added_removed_and_changed_and_is_empty_when_equal() {
        let a = resolve(&two(), "foo", &[], None, None, None, None).unwrap().snapshot();
        assert!(diff(&a, &a).is_empty());
        let mut b = a.clone();
        b.secrets = strs(&["b", "c"]);
        b.image = "other".into();
        b.memory_gb = Some(8);
        b.cpus = Some(4);
        b.disk_size_gb = Some(20);
        assert_eq!(
            diff(&a, &b),
            strs(&["secrets added: c", "secrets removed: a", "image: cbox-custom -> other", "cpus: default -> 4", "memory: default -> 8 GiB", "disk size: default -> 20 GB"])
        );
    }

    #[test]
    fn resume_warning_names_old_profile_and_flag_mismatch() {
        let a = resolve(&two(), "foo", &[], None, None, None, None).unwrap().snapshot();
        let mut b = a.clone();
        b.memory_gb = Some(8);
        let w = resume_warning(Some(&a), &b, None).unwrap();
        assert!(w.starts_with("cbox: warning: profile foo differs from this box (created with foo):\n  memory: default -> 8 GiB\n"), "{w}");
        assert!(w.ends_with("the box keeps its OLD settings until you run with -f/--force to recreate it."), "{w}");
        b.name = "bar".into();
        let w = resume_warning(Some(&a), &b, Some("bar")).unwrap();
        assert!(w.starts_with("cbox: warning: you asked for profile bar; this box was created with profile foo:\n"), "{w}");
        assert_eq!(resume_warning(Some(&a), &a, None), None);
        assert_eq!(resume_warning(None, &b, None), None);
    }

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
        assert_eq!(e, format!("invalid {}: line 2, column 1: not valid TOML or not a valid profile (unknown key or wrong type); fix or remove that entry", f.display()));
    }

    #[test]
    fn parse_errors_never_echo_the_file_contents() {
        let d = tmp("leak");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("profiles.toml");
        for (body, line) in [
            ("[default]\n\n[x]\ntoken = \"sk-DUMMY-VALUE-123\"\n", 4),
            ("[default]\ncpus = \"sk-DUMMY-VALUE-123\"\n", 2),
            ("[default]\n[x]\nimage = \"i\"\nsecrets = \"sk-DUMMY-VALUE-123\"\n", 4),
            ("[default]\nsk-DUMMY-VALUE-123 = = 1\n", 2),
        ] {
            std::fs::write(&f, body).unwrap();
            let e = format!("{:#}", Profiles::load(&f).unwrap_err());
            assert!(!e.contains("sk-DUMMY"), "{e}");
            assert!(e.contains(&f.display().to_string()) && e.contains(&format!("line {line},")), "{e}");
        }
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
    fn a_failed_fallback_create_keeps_the_hard_link_error() {
        use std::os::unix::fs::PermissionsExt;
        let f = tmp("fallback-ro").join("profiles.toml");
        let dir = f.parent().unwrap().to_path_buf();
        // The link "fails" and leaves the dir read-only, so the create_new retry fails too.
        let no_link = |_: &Path, _: &Path| {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
            Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
        };
        let e = create_new_via(&f, "[default]\n", no_link).unwrap_err();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let e = format!("{e:#}");
        assert!(e.contains("creating ") && e.contains("hard link failed first: unsupported"), "{e}");
        assert!(!f.exists());
        std::fs::remove_dir_all(&dir).ok();
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
