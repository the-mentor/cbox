use crate::config::secrets_index_path;
use crate::secretstore::{self, StoredSecret, open_default, parse_hosts, validate_env};
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
  cbox secret create github --host github.com,api.github.com --env GITHUB_TOKEN
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

pub fn run(cmd: SecretCmd) -> Result<()> {
    let store = open_default()?;
    let index = secrets_index_path().context("cannot locate config directory for the secrets index")?;
    match cmd {
        SecretCmd::Create { name, host, env } => {
            let hosts = parse_hosts(&host)?;
            validate_env(&env)?;
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
            let cells: Vec<[String; 4]> = rows
                .into_iter()
                .map(|r| match (r.hosts, r.env) {
                    (Some(h), Some(e)) => [r.name, h.join(","), e, store.label().into()],
                    _ => [r.name, "(missing value)".into(), "(missing value)".into(), store.label().into()],
                })
                .collect();
            let head = ["NAME", "HOSTS", "ENV", "STORE"].map(String::from);
            let w: Vec<usize> = (0..4).map(|i| cells.iter().chain([&head]).map(|r| r[i].len()).max().unwrap()).collect();
            for r in [&head].into_iter().chain(&cells) {
                println!("{:<w0$}  {:<w1$}  {:<w2$}  {}", r[0], r[1], r[2], r[3], w0 = w[0], w1 = w[1], w2 = w[2]);
            }
        }
        SecretCmd::Delete { name, yes } => {
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
        let mut ask = |a: &'static str| move |_: &str| Ok(a.to_string());
        assert!(!confirm_delete("n", false, &mut ask("\n")).unwrap());
        assert!(!confirm_delete("n", false, &mut ask("no\n")).unwrap());
        assert!(confirm_delete("n", false, &mut ask("y\n")).unwrap());
        assert!(confirm_delete("n", false, &mut ask("YES\n")).unwrap());
        assert!(confirm_delete("n", true, &mut |_| panic!("must not prompt")).unwrap());
    }
}
