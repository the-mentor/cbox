use crate::commands::secret::confirm_delete;
use crate::config::{profiles_path, secrets_index_path};
use crate::profiles::{self, Profile, Profiles, DEFAULT};
use crate::secretstore::Index;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::Path;

#[derive(Subcommand)]
pub enum ProfileCmd {
    /// Create a profile in profiles.toml.
    #[command(arg_required_else_help = true)]
    Create {
        name: String,
        /// Comma-separated secret names.
        #[arg(long)]
        secrets: Option<String>,
        #[arg(long)]
        image: Option<String>,
        /// Guest memory in GiB.
        #[arg(long, value_name = "GB")]
        memory: Option<u32>,
        #[arg(long)]
        cpus: Option<u8>,
        /// Rootfs disk size in GB.
        #[arg(long = "disk-size")]
        disk_size: Option<u64>,
    },
    /// List profiles.
    List,
    /// Change a profile; flags given replace that field.
    #[command(arg_required_else_help = true)]
    Edit {
        name: String,
        /// Replace the secret list (comma-separated).
        #[arg(long, conflicts_with_all = ["add_secrets", "remove_secrets"])]
        secrets: Option<String>,
        /// Append secret names (comma-separated).
        #[arg(long = "add-secrets")]
        add_secrets: Option<String>,
        /// Remove secret names (comma-separated).
        #[arg(long = "remove-secrets")]
        remove_secrets: Option<String>,
        #[arg(long)]
        image: Option<String>,
        #[arg(long, value_name = "GB")]
        memory: Option<u32>,
        #[arg(long)]
        cpus: Option<u8>,
        #[arg(long = "disk-size")]
        disk_size: Option<u64>,
        /// Stop setting these fields so cbox's built-ins apply (comma-separated).
        #[arg(long, value_enum, value_delimiter = ',', value_name = "FIELDS")]
        reset: Vec<ResetField>,
    },
    /// Delete a profile.
    #[command(arg_required_else_help = true)]
    Delete {
        name: String,
        /// Skip the confirmation prompt.
        #[arg(short, long)]
        yes: bool,
    },
    /// Check profiles.toml (one profile, or all) without changing it.
    Validate { name: Option<String> },
}

#[derive(Default)]
pub struct EditArgs {
    pub secrets: Option<Vec<String>>,
    pub add_secrets: Vec<String>,
    pub remove_secrets: Vec<String>,
    pub image: Option<String>,
    pub memory: Option<u32>,
    pub cpus: Option<u8>,
    pub disk_size: Option<u64>,
    /// Fields to stop setting, from `--reset`.
    pub reset: Vec<ResetField>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ResetField {
    Secrets,
    Image,
    Cpus,
    Memory,
    DiskSize,
}

impl ResetField {
    /// The name `--reset` and its sibling flag use (`disk-size`).
    fn flag(self) -> String {
        clap::ValueEnum::to_possible_value(&self).expect("no skipped variants").get_name().to_string()
    }
}

pub fn parse_names(s: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for item in s.split(',').map(str::trim) {
        if item.is_empty() {
            bail!("empty name in {s:?}");
        }
        if out.iter().any(|o| o == item) {
            bail!("duplicate name {item:?}");
        }
        out.push(item.to_string());
    }
    Ok(out)
}

fn with(existing: &Profiles, name: &str, p: Profile) -> Profiles {
    let mut m = existing.0.clone();
    m.insert(name.to_string(), p);
    Profiles(m)
}

pub fn apply_create(existing: &Profiles, name: &str, p: Profile) -> Result<Profiles> {
    if existing.get(name).is_some() {
        bail!("profile {name:?} already exists");
    }
    Ok(with(existing, name, p))
}

pub fn apply_edit(existing: &Profiles, name: &str, a: EditArgs) -> Result<Profiles> {
    let mut p = existing.get(name).with_context(|| format!("profile {name:?} not found"))?.clone();
    for f in &a.reset {
        let also_set = match f {
            ResetField::Secrets => a.secrets.is_some() || !a.add_secrets.is_empty() || !a.remove_secrets.is_empty(),
            ResetField::Image => a.image.is_some(),
            ResetField::Cpus => a.cpus.is_some(),
            ResetField::Memory => a.memory.is_some(),
            ResetField::DiskSize => a.disk_size.is_some(),
        };
        if also_set {
            if *f == ResetField::Secrets {
                bail!("--reset secrets conflicts with --secrets, --add-secrets and --remove-secrets");
            }
            bail!("--reset {0} conflicts with --{0}", f.flag());
        }
        match f {
            ResetField::Secrets => p.secrets.clear(),
            ResetField::Image => p.image = None,
            ResetField::Cpus => p.cpus = None,
            ResetField::Memory => p.memory = None,
            ResetField::DiskSize => p.disk_size = None,
        }
    }
    if let Some(s) = a.secrets {
        p.secrets = s;
    }
    for s in a.add_secrets {
        if p.secrets.contains(&s) {
            bail!("profile {name:?} already has secret {s:?}");
        }
        p.secrets.push(s);
    }
    for s in a.remove_secrets {
        let Some(i) = p.secrets.iter().position(|x| *x == s) else {
            bail!("profile {name:?} has no secret {s:?}");
        };
        p.secrets.remove(i);
    }
    p.image = a.image.or(p.image);
    p.memory = a.memory.or(p.memory);
    p.cpus = a.cpus.or(p.cpus);
    p.disk_size = a.disk_size.or(p.disk_size);
    Ok(with(existing, name, p))
}

pub fn apply_delete(existing: &Profiles, name: &str) -> Result<Profiles> {
    if name == DEFAULT {
        bail!("the default profile cannot be deleted");
    }
    if existing.get(name).is_none() {
        bail!("profile {name:?} not found");
    }
    let mut m = existing.0.clone();
    m.remove(name);
    Ok(Profiles(m))
}

fn format_list(p: &Profiles) -> String {
    let mut names: Vec<&String> = p.0.keys().collect();
    names.sort_by_key(|n| (n.as_str() != DEFAULT, n.as_str()));
    let mut out = String::new();
    for n in names {
        let x = &p.0[n];
        out.push_str(&format!("{n}\n"));
        let mut lines = Vec::new();
        if !x.secrets.is_empty() {
            lines.push(format!("secrets: {}", x.secrets.join(", ")));
        }
        if let Some(v) = &x.image {
            lines.push(format!("image: {v}"));
        }
        if let Some(v) = x.memory {
            lines.push(format!("memory: {v} GiB"));
        }
        if let Some(v) = x.cpus {
            lines.push(format!("cpus: {v}"));
        }
        if let Some(v) = x.disk_size {
            lines.push(format!("disk size: {v} GB"));
        }
        if lines.is_empty() {
            lines.push("(empty)".into());
        }
        for l in lines {
            out.push_str(&format!("  {l}\n"));
        }
    }
    out
}

fn known_secrets(index: &Path) -> Result<BTreeSet<String>> {
    Ok(Index::load(index)?.names)
}

fn check(name: &str, p: &Profile, index: &Path) -> Result<()> {
    let problems = profiles::validate(name, p, &known_secrets(index)?);
    if !problems.is_empty() {
        bail!("{}", problems.join("\n"));
    }
    Ok(())
}

pub fn run(cmd: ProfileCmd) -> Result<()> {
    let path = profiles_path().context("cannot locate config directory for profiles.toml")?;
    let index = secrets_index_path().context("cannot locate config directory for the secrets index")?;
    let mut ask = |q: &str| {
        print!("{q}");
        io::stdout().flush()?;
        let mut s = String::new();
        io::stdin().read_line(&mut s)?;
        Ok(s)
    };
    run_at(cmd, &path, &index, &mut ask, &mut io::stdout())
}

/// `ask` answers the delete prompt; `out` is stdout (both injected for tests).
fn run_at(cmd: ProfileCmd, path: &Path, index: &Path, ask: &mut dyn FnMut(&str) -> io::Result<String>, out: &mut dyn Write) -> Result<()> {
    match cmd {
        ProfileCmd::Create { name, secrets, image, memory, cpus, disk_size } => {
            let p = Profile {
                secrets: secrets.as_deref().map(parse_names).transpose()?.unwrap_or_default(),
                image, cpus, memory, disk_size,
            };
            check(&name, &p, index)?;
            apply_create(&Profiles::load(path)?, &name, p)?.save(path)?;
            writeln!(out, "Created profile \"{name}\".")?;
        }
        ProfileCmd::List => write!(out, "{}", format_list(&Profiles::load(path)?))?,
        ProfileCmd::Edit { name, secrets, add_secrets, remove_secrets, image, memory, cpus, disk_size, reset } => {
            let names = |s: Option<String>| s.as_deref().map(parse_names).transpose();
            let a = EditArgs {
                secrets: names(secrets)?,
                add_secrets: names(add_secrets)?.unwrap_or_default(),
                remove_secrets: names(remove_secrets)?.unwrap_or_default(),
                image, memory, cpus, disk_size,
                reset,
            };
            let next = apply_edit(&Profiles::load(path)?, &name, a)?;
            check(&name, next.get(&name).expect("just edited"), index)?;
            next.save(path)?;
            writeln!(out, "Updated profile \"{name}\".")?;
        }
        ProfileCmd::Delete { name, yes } => {
            let next = apply_delete(&Profiles::load(path)?, &name)?;
            let ok = confirm_delete("profile", &name, yes, ask)?;
            if !ok {
                writeln!(out, "Cancelled.")?;
                return Ok(());
            }
            next.save(path)?;
            writeln!(out, "Deleted \"{name}\".")?;
        }
        ProfileCmd::Validate { name } => {
            let problems = validate_file(path, index, name.as_deref())?;
            if problems.is_empty() {
                writeln!(out, "profiles.toml is valid")?;
            } else {
                for p in &problems {
                    writeln!(out, "{p}")?;
                }
                bail!("{} problem(s) in {}", problems.len(), path.display());
            }
        }
    }
    Ok(())
}

/// A parse error is the single problem; a named profile that is absent is an error.
fn validate_file(path: &Path, index: &Path, only: Option<&str>) -> Result<Vec<String>> {
    let all = match Profiles::load(path) {
        Ok(p) => p,
        Err(e) => return Ok(vec![e.to_string()]),
    };
    let known = known_secrets(index)?;
    if let Some(n) = only {
        let p = all.get(n).with_context(|| format!("profile {n:?} not found"))?;
        return Ok(profiles::validate(n, p, &known));
    }
    Ok(all.0.iter().flat_map(|(n, p)| profiles::validate(n, p, &known)).collect())
}

/// The warning to print when profiles.toml cannot be created; `None` when fine.
fn ensure_warning(path: &Path) -> Option<String> {
    let e = profiles::ensure_file(path).err()?;
    Some(format!("cbox: warning: could not create {} ({e:#}); using an empty default profile.", path.display()))
}

/// Create profiles.toml on first run. Never fails; call before `Cli::parse()`.
pub fn startup_ensure() {
    if let Some(w) = profiles_path().and_then(|p| ensure_warning(&p)) {
        eprintln!("{w}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Commands};
    use clap::Parser;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cbox-profcmd-{}-{tag}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn base() -> Profiles {
        let mut m = std::collections::BTreeMap::new();
        m.insert(DEFAULT.to_string(), Profile::default());
        m.insert("dev".to_string(), Profile { secrets: vec!["a".into(), "b".into()], cpus: Some(2), ..Default::default() });
        Profiles(m)
    }

    fn no_ask(_: &str) -> io::Result<String> {
        panic!("must not prompt")
    }

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_names_trims_keeps_order_and_rejects_empty_and_duplicates() {
        assert_eq!(parse_names(" b , a,c").unwrap(), v(&["b", "a", "c"]));
        assert!(parse_names("a,,b").is_err());
        assert!(parse_names("").is_err());
        assert!(parse_names("a,b,a").is_err());
    }

    #[test]
    fn apply_create_refuses_an_existing_name() {
        assert!(apply_create(&base(), "dev", Profile::default()).is_err());
        let p = Profile { image: Some("x".into()), ..Default::default() };
        assert_eq!(apply_create(&base(), "new", p.clone()).unwrap().get("new"), Some(&p));
    }

    #[test]
    fn apply_edit_replace_add_remove_and_value_flags() {
        let ed = |a: EditArgs| apply_edit(&base(), "dev", a);
        assert!(apply_edit(&base(), "nope", EditArgs::default()).is_err());
        let r = ed(EditArgs { secrets: Some(v(&["z"])), ..Default::default() }).unwrap();
        assert_eq!(r.get("dev").unwrap().secrets, v(&["z"]));
        let r = ed(EditArgs { add_secrets: v(&["c"]), ..Default::default() }).unwrap();
        assert_eq!(r.get("dev").unwrap().secrets, v(&["a", "b", "c"]));
        assert!(ed(EditArgs { add_secrets: v(&["a"]), ..Default::default() }).is_err());
        let r = ed(EditArgs { remove_secrets: v(&["a"]), ..Default::default() }).unwrap();
        assert_eq!(r.get("dev").unwrap().secrets, v(&["b"]));
        assert!(ed(EditArgs { remove_secrets: v(&["q"]), ..Default::default() }).is_err());
        let r = ed(EditArgs { image: Some("i".into()), memory: Some(8), disk_size: Some(20), ..Default::default() }).unwrap();
        let d = r.get("dev").unwrap();
        assert_eq!((d.image.as_deref(), d.memory, d.cpus, d.disk_size), (Some("i"), Some(8), Some(2), Some(20)), "unset cpus preserved");
        let r = ed(EditArgs { cpus: Some(4), ..Default::default() }).unwrap();
        assert_eq!(r.get("dev").unwrap().cpus, Some(4));
        assert_eq!(base().get("dev").unwrap().secrets, v(&["a", "b"]), "input untouched");
    }

    fn rs(s: &str) -> Vec<ResetField> {
        s.split(',').map(|x| <ResetField as clap::ValueEnum>::from_str(x, false).unwrap()).collect()
    }

    #[test]
    fn reset_clears_the_named_fields_and_only_those() {
        let full = Profile { image: Some("i".into()), memory: Some(8), cpus: Some(2), disk_size: Some(20), secrets: v(&["a"]) };
        let base = Profiles([("dev".to_string(), full)].into());
        let ed = |a: EditArgs| apply_edit(&base, "dev", a);
        let r = ed(EditArgs { reset: rs("image,disk-size"), ..Default::default() }).unwrap();
        let d = r.get("dev").unwrap();
        assert_eq!((d.image.as_deref(), d.memory, d.cpus, d.disk_size, d.secrets.clone()), (None, Some(8), Some(2), None, v(&["a"])));
        let r = ed(EditArgs { reset: rs("cpus,memory"), image: Some("j".into()), ..Default::default() }).unwrap();
        let d = r.get("dev").unwrap();
        assert_eq!((d.image.as_deref(), d.memory, d.cpus), (Some("j"), None, None), "reset one field, set another");
        let e = ed(EditArgs { reset: rs("cpus"), cpus: Some(4), ..Default::default() }).unwrap_err().to_string();
        assert_eq!(e, "--reset cpus conflicts with --cpus");
        let r = ed(EditArgs { reset: rs("secrets"), ..Default::default() }).unwrap();
        assert_eq!(r.get("dev").unwrap().secrets, Vec::<String>::new());
        assert_eq!(r.get("dev").unwrap().cpus, Some(2), "other fields kept");
        for conflicting in [
            EditArgs { secrets: Some(v(&["x"])), ..Default::default() },
            EditArgs { add_secrets: v(&["x"]), ..Default::default() },
            EditArgs { remove_secrets: v(&["a"]), ..Default::default() },
        ] {
            let a = EditArgs { reset: rs("secrets"), ..conflicting };
            assert!(ed(a).unwrap_err().to_string().contains("--reset secrets conflicts"));
        }
        assert!(apply_edit(&Profiles([("d".to_string(), Profile::default())].into()), "d", EditArgs { reset: rs("image"), ..Default::default() }).is_ok(), "resetting an unset field is fine");
    }

    #[test]
    fn clap_parses_reset_lists_and_rejects_unknown_fields() {
        let reset = |v: &str| match Cli::try_parse_from(["cbox", "profile", "edit", "foo", "--reset", v]).map(|c| c.command) {
            Ok(Commands::Profile { cmd: ProfileCmd::Edit { reset, .. } }) => Ok(reset),
            Ok(_) => unreachable!(),
            Err(e) => Err(e.to_string()),
        };
        assert_eq!(reset("secrets,image,cpus,memory,disk-size").unwrap().len(), 5);
        assert_eq!(reset("disk-size").unwrap(), vec![ResetField::DiskSize]);
        for bad in ["secret", "disk_size", "image,", "bogus"] {
            let e = reset(bad).unwrap_err();
            assert!(e.contains("possible values: secrets, image, cpus, memory, disk-size"), "{bad:?}: {e}");
        }
    }

    #[test]
    fn apply_delete_refuses_default_and_a_missing_name() {
        let e = apply_delete(&base(), DEFAULT).unwrap_err().to_string();
        assert_eq!(e, "the default profile cannot be deleted");
        assert!(apply_delete(&base(), "nope").is_err());
        assert!(apply_delete(&base(), "dev").unwrap().get("dev").is_none());
    }

    #[test]
    fn list_puts_default_first_and_shows_set_fields_only() {
        let out = format_list(&base());
        assert_eq!(out, "default\n  (empty)\ndev\n  secrets: a, b\n  cpus: 2\n");
    }

    #[test]
    fn validate_command_lists_every_problem_and_never_writes() {
        let d = tmp("validate");
        let (p, idx) = (d.join("profiles.toml"), d.join("secrets.json"));
        let body = "[default]\n[Bad]\ncpus = 0\n[dev]\nsecrets = [\"ghost\"]\n";
        std::fs::write(&p, body).unwrap();
        let problems = validate_file(&p, &idx, None).unwrap();
        assert!(problems.iter().any(|m| m.contains("profile Bad")), "{problems:?}");
        assert!(problems.iter().any(|m| m.contains("is not a stored secret")), "{problems:?}");
        assert!(run_at(ProfileCmd::Validate { name: None }, &p, &idx, &mut no_ask, &mut Vec::new()).is_err());
        assert!(validate_file(&p, &idx, Some("nope")).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
        std::fs::write(&p, "not toml [").unwrap();
        assert_eq!(validate_file(&p, &idx, None).unwrap().len(), 1);
    }

    #[test]
    fn create_edit_delete_round_trip_and_failed_create_writes_nothing() {
        let d = tmp("roundtrip");
        let (p, idx) = (d.join("profiles.toml"), d.join("secrets.json"));
        std::fs::write(&idx, r#"{"names":["a"]}"#).unwrap();
        let create = |n: &str, s: &str| ProfileCmd::Create { name: n.into(), secrets: Some(s.into()), image: None, memory: None, cpus: None, disk_size: None };
        assert!(run_at(create("dev", "ghost"), &p, &idx, &mut no_ask, &mut Vec::new()).is_err());
        assert!(!p.exists());
        run_at(create("dev", "a"), &p, &idx, &mut no_ask, &mut Vec::new()).unwrap();
        assert_eq!(Profiles::load(&p).unwrap().get("dev").unwrap().secrets, v(&["a"]));
        run_at(ProfileCmd::Delete { name: "dev".into(), yes: true }, &p, &idx, &mut no_ask, &mut Vec::new()).unwrap();
        assert!(Profiles::load(&p).unwrap().get("dev").is_none());
    }

    fn edit(name: &str, add: Option<&str>, remove: Option<&str>, cpus: Option<u8>) -> ProfileCmd {
        let s = |x: Option<&str>| x.map(String::from);
        ProfileCmd::Edit { name: name.into(), secrets: None, add_secrets: s(add), remove_secrets: s(remove), image: None, memory: None, cpus, disk_size: None, reset: vec![] }
    }

    /// A dir with `body` as profiles.toml and an index naming `a`.
    fn files(tag: &str, body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let d = tmp(tag);
        let (p, idx) = (d.join("profiles.toml"), d.join("secrets.json"));
        std::fs::write(&p, body).unwrap();
        std::fs::write(&idx, r#"{"names":["a"]}"#).unwrap();
        (p, idx)
    }

    #[test]
    fn a_failed_edit_leaves_the_file_byte_for_byte_unchanged() {
        let body = "# a comment that a save would drop\n[default]\n\n[dev]\nsecrets = [\"a\"]\n";
        let (p, idx) = files("edit-fail", body);
        let cmd = edit("dev", Some("ghost"), None, None);
        let e = run_at(cmd, &p, &idx, &mut no_ask, &mut Vec::new()).unwrap_err().to_string();
        assert!(e.contains("is not a stored secret"), "{e}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
        let e = run_at(edit("nope", None, None, None), &p, &idx, &mut no_ask, &mut Vec::new()).unwrap_err().to_string();
        assert_eq!(e, "profile \"nope\" not found");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
    }

    #[test]
    fn editing_only_cpus_still_fails_while_the_profile_names_a_deleted_secret() {
        // Pinned: edit validates the whole profile, so the deleted secret must be removed first.
        let body = "[default]\n[dev]\nsecrets = [\"deleted\"]\n";
        let (p, idx) = files("edit-cpus", body);
        let cmd = edit("dev", None, None, Some(4));
        let e = run_at(cmd, &p, &idx, &mut no_ask, &mut Vec::new()).unwrap_err().to_string();
        assert!(e.contains("is not a stored secret"), "{e}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
        let fix = edit("dev", None, Some("deleted"), Some(4));
        run_at(fix, &p, &idx, &mut no_ask, &mut Vec::new()).unwrap();
        assert_eq!(Profiles::load(&p).unwrap().get("dev").unwrap().cpus, Some(4));
    }

    #[test]
    fn validate_name_checks_only_that_profile() {
        let (p, idx) = files("validate-one", "[default]\n[bad]\ncpus = 0\n[good]\nsecrets = [\"a\"]\n");
        assert!(validate_file(&p, &idx, Some("good")).unwrap().is_empty());
        let mut out = Vec::new();
        run_at(ProfileCmd::Validate { name: Some("good".into()) }, &p, &idx, &mut no_ask, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "profiles.toml is valid\n");
        assert_eq!(validate_file(&p, &idx, Some("bad")).unwrap(), ["profile bad: cpus must be greater than 0"]);
        assert!(run_at(ProfileCmd::Validate { name: Some("bad".into()) }, &p, &idx, &mut no_ask, &mut Vec::new()).is_err());
    }

    #[test]
    fn validate_rejects_gh_basic_in_a_profile() {
        let (p, idx) = files("gh-basic", "[default]\nsecrets = [\"gh_basic\"]\n");
        let problems = validate_file(&p, &idx, None).unwrap();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("profile default: secrets entry 1") && problems[0].contains("reserved"), "{problems:?}");
    }

    #[test]
    fn a_zero_byte_secrets_index_is_an_error_not_an_empty_one() {
        let (p, idx) = files("empty-index", "[default]\n");
        std::fs::write(&idx, "").unwrap();
        let create = ProfileCmd::Create { name: "dev".into(), secrets: Some("a".into()), image: None, memory: None, cpus: None, disk_size: None };
        let e = format!("{:#}", run_at(create, &p, &idx, &mut no_ask, &mut Vec::new()).unwrap_err());
        assert!(e.starts_with(&format!("invalid index {}", idx.display())), "{e}");
        assert!(validate_file(&p, &idx, None).is_err());
    }

    #[test]
    fn declining_profile_delete_leaves_the_file_and_says_cancelled() {
        let body = "[default]\n\n[dev]\ncpus = 2\n";
        let (p, idx) = files("delete-no", body);
        let mut asked = String::new();
        let mut out = Vec::new();
        let cmd = ProfileCmd::Delete { name: "dev".into(), yes: false };
        run_at(cmd, &p, &idx, &mut |q| { asked = q.to_string(); Ok("n\n".into()) }, &mut out).unwrap();
        assert_eq!(asked, "Delete profile \"dev\"? [y/N] ");
        assert_eq!(String::from_utf8(out).unwrap(), "Cancelled.\n");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
    }

    #[test]
    fn startup_ensure_creates_the_file_once_and_survives_a_read_only_dir() {
        let d = tmp("startup");
        let p = d.join("cbox/profiles.toml");
        assert_eq!(ensure_warning(&p), None);
        std::fs::write(&p, "[default]\ncpus = 3\n").unwrap();
        assert_eq!(ensure_warning(&p), None);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[default]\ncpus = 3\n");
        // A regular file as the parent fails with ENOTDIR for any uid.
        let f = d.join("file");
        std::fs::write(&f, "").unwrap();
        let w = ensure_warning(&f.join("profiles.toml")).expect("warning");
        assert!(w.starts_with("cbox: warning: could not create ") && w.ends_with("using an empty default profile."), "{w}");
    }

    #[test]
    fn create_edit_delete_without_args_show_help() {
        for sub in ["create", "edit", "delete"] {
            let e = Cli::try_parse_from(["cbox", "profile", sub]).err().unwrap();
            assert_eq!(e.kind(), clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand, "{sub}");
        }
    }

    #[test]
    fn clap_parses_create_and_rejects_conflicting_edit_flags() {
        let cli = Cli::try_parse_from(["cbox", "profile", "create", "foo", "--secrets", "a,b"]).unwrap();
        assert!(matches!(cli.command, crate::Commands::Profile { cmd: ProfileCmd::Create { .. } }));
        assert!(Cli::try_parse_from(["cbox", "profile", "edit", "foo", "--secrets", "a", "--add-secrets", "b"]).is_err());
        assert!(Cli::try_parse_from(["cbox", "profile", "edit", "foo", "--add-secrets", "b", "--remove-secrets", "c"]).is_ok());
    }
}
