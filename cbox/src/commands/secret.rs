use crate::config::secrets_index_path;
use crate::secretstore::{self, EditChanges, EditOutcome, HostChange, ListRow, RowState, SecretStore, StoredSecret, open_default, parse_hosts, validate_env, validate_name};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use std::path::Path;
use std::io::{self, IsTerminal, Read, Write};

#[derive(Subcommand)]
pub enum SecretCmd {
    /// Store a secret in the OS secret store.
    #[command(after_help = "\
The value is read from a hidden prompt or from stdin and is never a command-line argument.

Examples:
  cbox secret create openai --host api.openai.com --env OPENAI_API_KEY
  cbox secret create openai --host api.openai.com,api.example.com --env OPENAI_API_KEY
  pbpaste | cbox secret create openai --host api.openai.com --env OPENAI_API_KEY")]
    Create {
        name: String,
        /// Comma-separated hosts the secret may be sent to.
        #[arg(long)]
        host: String,
        /// Environment variable name the box sees.
        #[arg(long)]
        env: String,
    },
    /// List stored secrets (never their values).
    List,
    /// Change a secret's hosts or variable name, or replace its value.
    #[command(after_help = "\
With no flags, the value is replaced (hidden prompt or stdin). With any flag, the value is left unchanged.

Examples:
  cbox secret edit openai --add-host api.openai.com
  cbox secret edit openai --host api.openai.com,api.example.com
  cbox secret edit openai --remove-host api.example.com --env OPENAI_KEY
  pbpaste | cbox secret edit openai")]
    Edit {
        name: String,
        /// Replace the whole host list (comma-separated).
        #[arg(long, conflicts_with_all = ["add_host", "remove_host"])]
        host: Option<String>,
        /// Add hosts (comma-separated).
        #[arg(long)]
        add_host: Option<String>,
        /// Remove hosts (comma-separated).
        #[arg(long)]
        remove_host: Option<String>,
        /// New environment variable name the box sees.
        #[arg(long)]
        env: Option<String>,
    },
    /// Delete a secret.
    Delete {
        name: String,
        /// Skip the confirmation prompt.
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
}

/// Strips exactly one trailing newline; rejects an empty value.
pub fn read_value(
    is_tty: bool,
    stdin: &mut dyn Read,
    prompt: &mut dyn FnMut() -> io::Result<String>,
) -> Result<String> {
    let mut v = if is_tty {
        prompt().context("cannot read secret value")?
    } else {
        let mut s = String::new();
        stdin.read_to_string(&mut s).context("cannot read secret value from stdin")?;
        s
    };
    if v.ends_with('\n') {
        v.pop();
        if v.ends_with('\r') {
            v.pop();
        }
    }
    if v.is_empty() {
        bail!("secret value is empty");
    }
    Ok(v)
}

pub(crate) fn confirm_delete(kind: &str, name: &str, yes: bool, ask: &mut dyn FnMut(&str) -> io::Result<String>) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    let a = ask(&format!("Delete {kind} \"{name}\"? [y/N] "))?;
    Ok(matches!(a.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Flags given -> the value is untouched and never read; no flags -> read a new value.
fn edit_changes(
    host: Option<&str>,
    add: Option<&str>,
    remove: Option<&str>,
    env: Option<&str>,
    read: &mut dyn FnMut() -> Result<String>,
) -> Result<EditChanges> {
    if let Some(e) = env {
        validate_env(e)?;
    }
    let list = |s: Option<&str>| s.map(parse_hosts).transpose();
    let hosts = HostChange {
        replace: list(host)?,
        add: list(add)?.unwrap_or_default(),
        remove: list(remove)?.unwrap_or_default(),
    };
    let flagged = env.is_some() || host.is_some() || add.is_some() || remove.is_some();
    let value = if flagged { None } else { Some(read()?) };
    Ok(EditChanges { value, hosts, env: env.map(String::from) })
}

fn hosts_line(o: &EditOutcome) -> Option<String> {
    (o.old_hosts != o.new_hosts).then(|| format!("hosts: {} -> {}", o.old_hosts.join(", "), o.new_hosts.join(", ")))
}

fn format_list(rows: &[ListRow], label: &str) -> String {
    let cells: Vec<[String; 4]> = rows
        .iter()
        .map(|r| match &r.state {
            RowState::Ok { hosts, env } => [r.name.clone(), hosts.join(","), env.clone(), label.into()],
            RowState::Missing => [r.name.clone(), "(missing value)".into(), "(missing value)".into(), label.into()],
            RowState::Unreadable => [r.name.clone(), format!("(unreadable: run cbox secret delete {})", r.name), "-".into(), label.into()],
        })
        .collect();
    let head = ["NAME", "HOSTS", "ENV", "STORE"].map(String::from);
    let w: Vec<usize> = (0..3).map(|i| cells.iter().chain([&head]).map(|r| r[i].len()).max().unwrap()).collect();
    [&head].into_iter().chain(&cells).fold(String::new(), |mut out, r| {
        out += &format!("{:<w0$}  {:<w1$}  {:<w2$}  {}\n", r[0], r[1], r[2], r[3], w0 = w[0], w1 = w[1], w2 = w[2]);
        out
    })
}

/// Echo is fully off (no `*`), so the prompt must say so or a paste looks like a hang.
fn prompt(label: &str, name: &str) -> String {
    format!("{label} \"{name}\" (input is hidden, press Enter when done): ")
}

/// Fails before `read` is ever called when the secret cannot be edited.
fn run_edit(
    store: &dyn SecretStore,
    index: &Path,
    name: &str,
    [host, add, remove, env]: [Option<&str>; 4],
    read: &mut dyn FnMut() -> Result<String>,
) -> Result<EditOutcome> {
    secretstore::check_editable(store, name)?;
    let c = edit_changes(host, add, remove, env, read)?;
    secretstore::edit(store, index, name, c)
}

/// Stderr warning for `secret delete`, or `None` when no profile uses the secret.
fn delete_warning(profiles: &Result<crate::profiles::Profiles>, name: &str) -> Option<String> {
    match profiles {
        Ok(p) => {
            let used = crate::profiles::profiles_using(p, name);
            (!used.is_empty()).then(|| {
                format!("cbox: warning: secret \"{name}\" is used by profile(s): {}. A new box from them will fail at \"cbox up\" (a resumed one only warns) until you remove it from them.", used.join(", "))
            })
        }
        Err(e) => Some(format!("cbox: warning: could not check profiles.toml for uses of this secret ({e}).")),
    }
}

pub fn run(cmd: SecretCmd) -> Result<()> {
    let store = open_default()?;
    let index = secrets_index_path().context("cannot locate config directory for the secrets index")?;
    match cmd {
        SecretCmd::Create { name, host, env } => {
            validate_name(&name)?;
            let hosts = parse_hosts(&host)?;
            validate_env(&env)?;
            secretstore::check_absent(&*store, &index, &name)?;
            let value = read_value(
                io::stdin().is_terminal(),
                &mut io::stdin(),
                &mut || rpassword::prompt_password(prompt("Value for", &name)),
            )?;
            secretstore::create(&*store, &index, &name, StoredSecret { value, hosts: hosts.clone(), env })?;
            println!("Saved \"{name}\" in {}, allowed hosts: {}", store.label(), hosts.join(", "));
        }
        SecretCmd::List => {
            let rows = secretstore::list(&*store, &index)?;
            if rows.is_empty() {
                println!("No secrets yet. Add one with `cbox secret create <NAME> --host <HOSTS> --env <VAR>`.");
                return Ok(());
            }
            print!("{}", format_list(&rows, store.label()));
        }
        SecretCmd::Edit { name, host, add_host, remove_host, env } => {
            let o = run_edit(&*store, &index, &name, [host.as_deref(), add_host.as_deref(), remove_host.as_deref(), env.as_deref()], &mut || {
                read_value(io::stdin().is_terminal(), &mut io::stdin(), &mut || {
                    rpassword::prompt_password(prompt("New value for", &name))
                })
            })?;
            if let Some(l) = hosts_line(&o) {
                println!("{l}");
            }
            println!("Updated \"{name}\" in {}", store.label());
        }
        SecretCmd::Delete { name, yes } => {
            if !secretstore::exists(&*store, &index, &name)? {
                bail!("secret {name:?} not found");
            }
            if let Some(w) = crate::config::profiles_path().and_then(|p| delete_warning(&crate::profiles::Profiles::load(&p), &name)) {
                eprintln!("{w}");
            }
            let ok = confirm_delete("secret", &name, yes, &mut |q| {
                print!("{q}");
                io::stdout().flush()?;
                let mut s = String::new();
                io::stdin().read_line(&mut s)?;
                Ok(s)
            })?;
            if !ok {
                println!("Cancelled.");
                return Ok(());
            }
            secretstore::delete(&*store, &index, &name)?;
            println!("Deleted \"{name}\".");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// In-memory store; names in `corrupt` fail like an unreadable entry (names the secret, no content).
    #[derive(Default)]
    struct Fake {
        m: RefCell<HashMap<String, StoredSecret>>,
        corrupt: Vec<&'static str>,
    }
    impl SecretStore for Fake {
        fn label(&self) -> &'static str {
            "fake"
        }
        fn max_entry_bytes(&self) -> Option<usize> {
            None
        }
        fn get(&self, name: &str) -> Result<Option<StoredSecret>> {
            if self.corrupt.contains(&name) {
                bail!("secret {name:?} in fake: invalid stored secret");
            }
            Ok(self.m.borrow().get(name).map(|s| StoredSecret { value: s.value.clone(), hosts: s.hosts.clone(), env: s.env.clone() }))
        }
        fn put(&self, name: &str, s: &StoredSecret) -> Result<()> {
            self.m.borrow_mut().insert(name.into(), StoredSecret { value: s.value.clone(), hosts: s.hosts.clone(), env: s.env.clone() });
            Ok(())
        }
        fn delete(&self, name: &str) -> Result<bool> {
            Ok(self.m.borrow_mut().remove(name).is_some())
        }
    }

    fn idx(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("cbox-edit-{}-{tag}.json", std::process::id()))
    }
    fn seeded() -> Fake {
        let f = Fake { corrupt: vec!["bad"], ..Default::default() };
        f.put("openai", &StoredSecret { value: "old".into(), hosts: vec!["a.com".into()], env: "K".into() }).unwrap();
        f
    }
    fn must_not_read() -> Result<String> {
        panic!("reader must not be called")
    }

    #[test]
    fn edit_fails_before_reading_when_the_secret_cannot_be_edited() {
        let st = seeded();
        // "listed" is in the index but has no store entry.
        let index = idx("listed");
        std::fs::write(&index, r#"{"version":1,"names":["listed"]}"#).unwrap();
        for (name, want) in [
            ("missing", "secret \"missing\" not found; run cbox secret create missing"),
            ("listed", "secret \"listed\" not found; run cbox secret create listed"),
            ("bad", "secret \"bad\" in fake"),
            ("Bad_Name", ""),
            ("gh", ""),
        ] {
            let e = run_edit(&st, &index, name, [None; 4], &mut must_not_read).unwrap_err().to_string();
            assert!(e.contains(want) && !e.contains("LEAKME"), "{name}: {e}");
            run_edit(&st, &index, name, [None, None, None, Some("E")], &mut must_not_read).unwrap_err();
        }
        let _ = std::fs::remove_file(&index);
    }

    #[test]
    fn edit_reads_once_without_flags_and_never_with_one() {
        let st = seeded();
        let index = idx("ok");
        let mut calls = 0;
        run_edit(&st, &index, "openai", [None; 4], &mut || {
            calls += 1;
            Ok("new".into())
        })
        .unwrap();
        assert_eq!((calls, st.get("openai").unwrap().unwrap().value.as_str()), (1, "new"));
        run_edit(&st, &index, "openai", [None, None, None, Some("E")], &mut must_not_read).unwrap();
        let s = st.get("openai").unwrap().unwrap();
        assert_eq!((s.value.as_str(), s.env.as_str()), ("new", "E"));
        let _ = std::fs::remove_file(&index);
    }

    #[test]
    fn prompts_say_input_is_hidden() {
        assert_eq!(prompt("Value for", "n"), "Value for \"n\" (input is hidden, press Enter when done): ");
        assert_eq!(prompt("New value for", "n"), "New value for \"n\" (input is hidden, press Enter when done): ");
    }

    #[derive(Parser)]
    struct W {
        #[command(subcommand)]
        cmd: SecretCmd,
    }

    fn rv(input: &str) -> Result<String> {
        read_value(false, &mut input.as_bytes(), &mut || unreachable!())
    }

    #[test]
    fn read_value_strips_one_trailing_newline() {
        assert_eq!(rv("abc\n").unwrap(), "abc");
        assert_eq!(rv("abc\r\n").unwrap(), "abc");
        assert_eq!(rv("abc\n\n").unwrap(), "abc\n");
    }

    #[test]
    fn read_value_rejects_empty() {
        assert!(rv("").is_err());
        assert!(rv("\n").is_err());
        assert!(read_value(true, &mut io::empty(), &mut || Ok("\n".into())).is_err());
    }

    #[test]
    fn read_value_uses_prompt_on_tty() {
        assert_eq!(read_value(true, &mut io::empty(), &mut || Ok("pw".into())).unwrap(), "pw");
    }

    #[test]
    fn create_requires_env_and_host() {
        let e = W::try_parse_from(["w", "create", "n", "--host", "a.com"]).err().unwrap().to_string();
        assert!(e.contains("--env"), "{e}");
        let e = W::try_parse_from(["w", "create", "n", "--env", "X"]).err().unwrap().to_string();
        assert!(e.contains("--host"), "{e}");
    }

    #[test]
    fn create_help_has_examples_that_all_pass_env() {
        let mut cmd = W::command();
        let help = cmd.find_subcommand_mut("create").unwrap().render_long_help().to_string();
        assert!(help.contains("Examples:"), "{help}");
        assert!(help.contains("never a command-line argument"), "{help}");
        let ex: Vec<_> = help.lines().filter(|l| l.contains("cbox secret create")).collect();
        assert_eq!(ex.len(), 3, "{help}");
        assert!(ex.iter().all(|l| l.contains("--env")), "{help}");
    }

    #[test]
    fn delete_without_y_needs_a_yes_answer() {
        let ask = |a: &'static str| move |_: &str| Ok(a.to_string());
        assert!(!confirm_delete("secret", "n", false, &mut ask("\n")).unwrap());
        assert!(!confirm_delete("secret", "n", false, &mut ask("no\n")).unwrap());
        assert!(confirm_delete("secret", "n", false, &mut ask("y\n")).unwrap());
        assert!(confirm_delete("secret", "n", false, &mut ask("YES\n")).unwrap());
        assert!(confirm_delete("secret", "n", true, &mut |_| panic!("must not prompt")).unwrap());
    }

    #[test]
    fn confirm_delete_does_not_delete_on_eof() {
        assert!(!confirm_delete("secret", "n", false, &mut |_| Ok(String::new())).unwrap());
    }

    #[test]
    fn delete_warning_covers_used_unused_and_load_failure() {
        let mut m = std::collections::BTreeMap::new();
        m.insert("b".to_string(), crate::profiles::Profile { secrets: vec!["k".into()], ..Default::default() });
        m.insert("a".to_string(), crate::profiles::Profile { secrets: vec!["k".into()], ..Default::default() });
        let p = Ok(crate::profiles::Profiles(m));
        assert_eq!(
            delete_warning(&p, "k").as_deref(),
            Some("cbox: warning: secret \"k\" is used by profile(s): a, b. A new box from them will fail at \"cbox up\" (a resumed one only warns) until you remove it from them.")
        );
        assert_eq!(delete_warning(&p, "other"), None);
        let e = Err(anyhow::anyhow!("boom"));
        assert_eq!(delete_warning(&e, "k").as_deref(), Some("cbox: warning: could not check profiles.toml for uses of this secret (boom)."));
    }

    fn row(name: &str, state: RowState) -> ListRow {
        ListRow { name: name.into(), state }
    }

    fn ok(hosts: &[&str], env: &str) -> RowState {
        RowState::Ok { hosts: hosts.iter().map(|s| s.to_string()).collect(), env: env.into() }
    }

    #[test]
    fn list_table_aligns_columns_and_marks_missing_values() {
        let out = format_list(
            &[row("openai", ok(&["a.com", "b.com"], "K")), row("x", RowState::Missing), row("bad", RowState::Unreadable)],
            "macOS Keychain",
        );
        assert_eq!(
            out,
            "NAME    HOSTS                                     ENV              STORE\n\
             openai  a.com,b.com                               K                macOS Keychain\n\
             x       (missing value)                           (missing value)  macOS Keychain\n\
             bad     (unreadable: run cbox secret delete bad)  -                macOS Keychain\n"
        );
    }

    #[test]
    fn create_and_edit_validate_the_name_before_reading_the_value() {
        for n in ["gh", "gh_basic", "Bad"] {
            assert!(validate_name(n).is_err(), "{n}");
        }
        validate_name("ok").unwrap();
    }

    fn no_read() -> Result<String> {
        panic!("must not read the value")
    }

    #[test]
    fn edit_with_only_host_flags_keeps_the_value_and_does_not_prompt() {
        for (h, a, r, e) in [
            (Some("a.com"), None, None, None),
            (None, Some("a.com"), None, None),
            (None, None, Some("a.com"), None),
            (None, None, None, Some("NEW")),
        ] {
            let c = edit_changes(h, a, r, e, &mut no_read).unwrap();
            assert!(c.value.is_none());
        }
        let c = edit_changes(None, Some("a.com, b.com"), None, Some("NEW"), &mut no_read).unwrap();
        assert!(c.hosts.add == ["a.com", "b.com"] && c.hosts.replace.is_none());
        assert_eq!(c.env.as_deref(), Some("NEW"));
        assert!(edit_changes(None, None, None, Some("1bad"), &mut no_read).is_err());
    }

    #[test]
    fn edit_rejects_url_hosts_in_every_host_flag() {
        let u = Some("https://api.openai.com/v1");
        assert!(edit_changes(u, None, None, None, &mut || Ok(String::new())).is_err());
        assert!(edit_changes(None, u, None, None, &mut || Ok(String::new())).is_err());
        assert!(edit_changes(None, None, u, None, &mut || Ok(String::new())).is_err());
    }

    #[test]
    fn edit_with_no_flags_replaces_the_value() {
        let c = edit_changes(None, None, None, None, &mut || Ok("newval".into())).unwrap();
        assert_eq!(c.value.as_deref(), Some("newval"));
        assert!(c.hosts.replace.is_none() && c.hosts.add.is_empty() && c.hosts.remove.is_empty() && c.env.is_none());
    }

    #[test]
    fn host_conflicts_with_add_and_remove_host() {
        assert!(W::try_parse_from(["w", "edit", "n", "--host", "a.com", "--add-host", "b.com"]).is_err());
        assert!(W::try_parse_from(["w", "edit", "n", "--host", "a.com", "--remove-host", "b.com"]).is_err());
        assert!(W::try_parse_from(["w", "edit", "n", "--add-host", "a.com", "--remove-host", "b.com"]).is_ok());
    }

    #[test]
    fn edit_prints_old_and_new_hosts_only_when_they_change() {
        let o = |a: &[&str], b: &[&str]| secretstore::EditOutcome {
            old_hosts: a.iter().map(|s| s.to_string()).collect(),
            new_hosts: b.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(hosts_line(&o(&["a.com", "b.com"], &["a.com", "c.com"])).as_deref(), Some("hosts: a.com, b.com -> a.com, c.com"));
        assert_eq!(hosts_line(&o(&["a.com"], &["a.com"])), None);
    }
}
