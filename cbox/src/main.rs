mod attach;
mod boxopts;
mod client;
mod commands;
mod config;
mod env;
mod envfile;
mod hookfwd;
mod naming;
mod profiles;
mod proto;
mod secrets;
mod secretstore;
mod server;
mod sidecar;
mod stdin_reader;
#[cfg(test)]
mod test_env_lock;
mod terminal_guard;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "cbox",
    about = "Drive BoxLite micro-VMs for this repo",
    // `version` with no value takes CARGO_PKG_VERSION, so `-V` tracks
    // Cargo.toml automatically rather than needing a hand-edited string.
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a box and attach this terminal to it.
    Up {
        name: Option<String>,
        #[arg(short, long)]
        force: bool,
        #[arg(short = 'c', long = "cwd")]
        cwd_mount: bool,
        #[arg(short = 'v', long = "volume")]
        volumes: Vec<String>,
        #[arg(short = 'e', long = "env")]
        env_flags: Vec<String>,
        /// Image to run. Defaults to the profile's image, then cbox-custom.
        #[arg(short = 'i', long)]
        image: Option<String>,
        /// Profile to apply (see `cbox profile`). Defaults to the profile the
        /// box was created with, else `default`.
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
        /// NAME (stored with `cbox secret create`) or NAME=ENV_VAR@host[,host...]
        #[arg(long = "secret")]
        secret_flags: Vec<String>,
        /// KEY=VALUE file to load before resolving credentials. Defaults to
        /// $CBOX_ENV_FILE, then ~/.config/cbox/env.
        #[arg(long = "env-file")]
        env_file: Option<PathBuf>,
        /// Container rootfs disk size in GB. The COW overlay is sparse and
        /// grows with actual usage; the virtual size is max(this, base image
        /// size), so smaller values are ignored. Defaults to 10GB, which
        /// gives headroom for in-box docker pull/apt/npm/build caches.
        #[arg(long = "disk-size")]
        disk_size: Option<u64>,
        /// Guest memory in GiB. Defaults to 4 (BoxLite's own default is 1,
        /// too little to build Rust or large JS projects in the box).
        #[arg(long = "memory", value_name = "GB")]
        memory: Option<u32>,
        /// Guest vCPU count. Defaults to 2.
        #[arg(long = "cpus")]
        cpus: Option<u8>,
        /// Let the box outlive this session so `cbox exec` can reach it
        /// later. Without this, closing the terminal lets boxlite's own
        /// watchdog stop the VM -- the disk and box record survive, and a
        /// later `cbox up` resumes it (a cold boot, not a suspend/resume).
        /// Mirrors `boxlite run`'s `-d`.
        #[arg(short = 'd', long = "detach")]
        detach: bool,
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// Print the box name that would be used here, and why.
    Name { name: Option<String> },
    /// Open a session in a running box.
    Exec {
        name: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// Stop and remove a box.
    Down { name: Option<String> },
    /// List boxes across every per-name home.
    List {
        /// Include stopped boxes.
        #[arg(short, long)]
        all: bool,
    },
    /// Manage secrets in the OS secret store.
    Secret {
        #[command(subcommand)]
        cmd: commands::secret::SecretCmd,
    },
    /// Manage named bundles of `up` choices (profiles.toml).
    Profile {
        #[command(subcommand)]
        cmd: commands::profile::ProfileCmd,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Before parse: clap exits on --help, which must still create the file.
    commands::profile::startup_ensure();
    let cli = Cli::try_parse().unwrap_or_else(|e| {
        // Asking for help by leaving arguments out is not a failure: clap exits 2 for it, which
        // makes `just` print "Recipe failed". Real usage errors keep their exit code.
        if e.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
            e.print().ok();
            std::process::exit(0);
        }
        e.exit()
    });
    match cli.command {
        Commands::Name { name } => {
            let cwd = std::env::current_dir().context("cannot read current directory")?;
            let resolved = naming::resolve(name.as_deref(), &cwd);
            println!("{}  (derived: {})", resolved.name, resolved.source.describe());
        }
        Commands::Up {
            name, force, cwd_mount, volumes, env_flags, image, profile, config, secret_flags, env_file, cmd,
            disk_size, memory, cpus, detach,
        } => {
            commands::up::run(commands::up::UpArgs {
                name, force, cwd_mount, volumes, env_flags, image, profile, config, secret_flags, env_file,
                cmd, disk_size_gb: disk_size, memory_gb: memory, cpus, detach,
            })
            .await?;
        }
        Commands::Exec { name, config, cmd } => commands::exec::run(name, cmd, config).await?,
        Commands::Down { name } => commands::down::run(name).await?,
        Commands::List { all } => commands::list::run(all).await?,
        Commands::Secret { cmd } => commands::secret::run(cmd)?,
        Commands::Profile { cmd } => commands::profile::run(cmd)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_up(args: &[&str]) -> Result<(Option<u32>, Option<u8>), clap::Error> {
        let cli = Cli::try_parse_from(["cbox", "up"].iter().chain(args))?;
        match cli.command {
            Commands::Up { memory, cpus, .. } => Ok((memory, cpus)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn up_image_and_profile_are_optional() {
        let up = |a: &[&str]| match Cli::try_parse_from(["cbox", "up"].iter().chain(a)).unwrap().command {
            Commands::Up { image, profile, .. } => (image, profile),
            _ => unreachable!(),
        };
        assert_eq!(up(&[]), (None, None));
        assert_eq!(up(&["-i", "x", "--profile", "foo"]), (Some("x".into()), Some("foo".into())));
    }

    #[test]
    fn memory_is_whole_gib_and_cpus_a_count() {
        assert_eq!(parse_up(&["--memory", "8", "--cpus", "4"]).unwrap(), (Some(8), Some(4)));
        assert_eq!(parse_up(&[]).unwrap(), (None, None));
    }

    #[test]
    fn an_out_of_range_cpu_count_is_rejected_by_clap_naming_the_flag() {
        let err = parse_up(&["--cpus", "300"]).unwrap_err().to_string();
        assert!(err.contains("--cpus"), "{err}");
    }
}
