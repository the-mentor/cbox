use crate::config::secrets_index_path;
use crate::secretstore::{self, EditChanges, EditOutcome, HostChange, ListRow, StoredSecret, open_default, parse_hosts, validate_env, validate_name};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
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

fn confirm_delete(name: &str, yes: bool, ask: &mut dyn FnMut(&str) -> io::Result<String>) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    let a = ask(&format!("Delete secret \"{name}\"? [y/N] "))?;
    Ok(matches!(a.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Fails fast, before any hidden prompt.
fn check_name(name: &str) -> Result<()> {
    validate_name(name)
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
    let hosts = match (host, add, remove) {
        (Some(h), _, _) => HostChange::Replace(parse_hosts(h)?),
        (_, Some(a), Some(r)) => HostChange::AddRemove(parse_hosts(a)?, parse_hosts(r)?),
        (_, Some(a), _) => HostChange::Add(parse_hosts(a)?),
        (_, _, Some(r)) => HostChange::Remove(parse_hosts(r)?),
        _ => HostChange::Keep,
    };
    let flagged = env.is_some() || !matches!(hosts, HostChange::Keep);
    let value = if flagged { None } else { Some(read()?) };
    Ok(EditChanges { value, hosts, env: env.map(String::from) })
}

fn hosts_line(o: &EditOutcome) -> Option<String> {
    (o.old_hosts != o.new_hosts).then(|| format!("hosts: {} -> {}", o.old_hosts.join(", "), o.new_hosts.join(", ")))
}

fn format_list(rows: &[ListRow], label: &str) -> String {
    let cells: Vec<[String; 4]> = rows
        .iter()
        .map(|r| match (&r.hosts, &r.env) {
            (Some(h), Some(e)) => [r.name.clone(), h.join(","), e.clone(), label.into()],
            _ => [r.name.clone(), "(missing value)".into(), "(missing value)".into(), label.into()],
        })
        .collect();
    let head = ["NAME", "HOSTS", "ENV", "STORE"].map(String::from);
    let w: Vec<usize> = (0..3).map(|i| cells.iter().chain([&head]).map(|r| r[i].len()).max().unwrap()).collect();
    [&head].into_iter().chain(&cells).fold(String::new(), |mut out, r| {
        out += &format!("{:<w0$}  {:<w1$}  {:<w2$}  {}\n", r[0], r[1], r[2], r[3], w0 = w[0], w1 = w[1], w2 = w[2]);
        out
    })
}

pub fn run(cmd: SecretCmd) -> Result<()> {
    let store = open_default()?;
    let index = secrets_index_path().context("cannot locate config directory for the secrets index")?;
    match cmd {
        SecretCmd::Create { name, host, env } => {
            check_name(&name)?;
            let hosts = parse_hosts(&host)?;
            validate_env(&env)?;
            secretstore::check_absent(&*store, &index, &name)?;
            let value = read_value(
                io::stdin().is_terminal(),
                &mut io::stdin(),
                &mut || rpassword::prompt_password(format!("Value for \"{name}\": ")),
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
            check_name(&name)?;
            let c = edit_changes(host.as_deref(), add_host.as_deref(), remove_host.as_deref(), env.as_deref(), &mut || {
                read_value(io::stdin().is_terminal(), &mut io::stdin(), &mut || {
                    rpassword::prompt_password(format!("New value for \"{name}\": "))
                })
            })?;
            let o = secretstore::edit(&*store, &name, c)?;
            if let Some(l) = hosts_line(&o) {
                println!("{l}");
            }
            println!("Updated \"{name}\" in {}", store.label());
        }
        SecretCmd::Delete { name, yes } => {
            if !secretstore::exists(&*store, &index, &name)? {
                bail!("secret {name:?} not found");
            }
            let ok = confirm_delete(&name, yes, &mut |q| {
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
        assert!(!confirm_delete("n", false, &mut ask("\n")).unwrap());
        assert!(!confirm_delete("n", false, &mut ask("no\n")).unwrap());
        assert!(confirm_delete("n", false, &mut ask("y\n")).unwrap());
        assert!(confirm_delete("n", false, &mut ask("YES\n")).unwrap());
        assert!(confirm_delete("n", true, &mut |_| panic!("must not prompt")).unwrap());
    }

    #[test]
    fn confirm_delete_does_not_delete_on_eof() {
        assert!(!confirm_delete("n", false, &mut |_| Ok(String::new())).unwrap());
    }

    fn row(name: &str, h: Option<&[&str]>, e: Option<&str>) -> secretstore::ListRow {
        secretstore::ListRow { name: name.into(), hosts: h.map(|h| h.iter().map(|s| s.to_string()).collect()), env: e.map(String::from) }
    }

    #[test]
    fn list_table_aligns_columns_and_marks_missing_values() {
        let out = format_list(&[row("openai", Some(&["a.com", "b.com"]), Some("K")), row("x", None, None)], "macOS Keychain");
        assert_eq!(
            out,
            "NAME    HOSTS            ENV              STORE\n\
             openai  a.com,b.com      K                macOS Keychain\n\
             x       (missing value)  (missing value)  macOS Keychain\n"
        );
    }

    #[test]
    fn create_and_edit_validate_the_name_before_reading_the_value() {
        for n in ["gh", "gh_basic", "Bad"] {
            assert!(check_name(n).is_err(), "{n}");
        }
        check_name("ok").unwrap();
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
        assert!(matches!(c.hosts, HostChange::Add(ref h) if h == &["a.com", "b.com"]));
        assert_eq!(c.env.as_deref(), Some("NEW"));
        assert!(edit_changes(None, None, None, Some("1bad"), &mut no_read).is_err());
    }

    #[test]
    fn edit_with_no_flags_replaces_the_value() {
        let c = edit_changes(None, None, None, None, &mut || Ok("newval".into())).unwrap();
        assert_eq!(c.value.as_deref(), Some("newval"));
        assert!(matches!(c.hosts, HostChange::Keep) && c.env.is_none());
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
