mod attach;
mod boxopts;
mod client;
mod commands;
mod config;
mod env;
mod envfile;
mod hookfwd;
mod naming;
mod netdrift;
mod netpolicy;
mod proto;
mod secrets;
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
        #[arg(short = 'i', long, default_value = "cbox-custom")]
        image: String,
        #[arg(long)]
        config: Option<PathBuf>,
        /// NAME=ENV_VAR@host[,host...]
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
        /// Restrict egress to this rule (repeatable): @preset (github, npm,
        /// crates, pypi, debian), a host, *.domain, an IP or a CIDR. The
        /// gateway is always added. Without it the box is unrestricted.
        #[arg(long = "allow-net", value_name = "RULE")]
        allow_net: Vec<String>,
        /// `disabled` removes the box's network entirely.
        #[arg(long = "network", value_name = "MODE", value_parser = ["disabled"],
              conflicts_with = "allow_net")]
        network: Option<String>,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Name { name } => {
            let cwd = std::env::current_dir().context("cannot read current directory")?;
            let resolved = naming::resolve(name.as_deref(), &cwd);
            println!("{}  (derived: {})", resolved.name, resolved.source.describe());
        }
        Commands::Up {
            name, force, cwd_mount, volumes, env_flags, image, config, secret_flags, env_file, cmd,
            disk_size, memory, cpus, detach, allow_net, network,
        } => {
            commands::up::run(commands::up::UpArgs {
                name, force, cwd_mount, volumes, env_flags, image, config, secret_flags, env_file,
                cmd, disk_size_gb: disk_size, memory_gb: memory, cpus, detach,
                allow_net, network_disabled: network.is_some(),
            })
            .await?;
        }
        Commands::Exec { name, config, cmd } => commands::exec::run(name, cmd, config).await?,
        Commands::Down { name } => commands::down::run(name).await?,
        Commands::List { all } => commands::list::run(all).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_net(args: &[&str]) -> Result<(Vec<String>, Option<String>), clap::Error> {
        let cli = Cli::try_parse_from(["cbox", "up"].iter().chain(args))?;
        match cli.command {
            Commands::Up { allow_net, network, .. } => Ok((allow_net, network)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn allow_net_is_repeatable() {
        let (rules, net) = parse_net(&["--allow-net", "@github", "--allow-net", "10.0.0.0/8"]).unwrap();
        assert_eq!(rules, vec!["@github".to_string(), "10.0.0.0/8".to_string()]);
        assert_eq!(net, None);
    }

    #[test]
    fn network_accepts_only_disabled() {
        assert_eq!(parse_net(&["--network", "disabled"]).unwrap().1, Some("disabled".into()));
        assert!(parse_net(&["--network", "open"]).is_err());
    }

    #[test]
    fn allow_net_and_network_disabled_conflict() {
        let err = parse_net(&["--allow-net", "@npm", "--network", "disabled"]).unwrap_err().to_string();
        assert!(err.contains("--network") || err.contains("--allow-net"), "{err}");
    }

    fn parse_up(args: &[&str]) -> Result<(Option<u32>, Option<u8>), clap::Error> {
        let cli = Cli::try_parse_from(["cbox", "up"].iter().chain(args))?;
        match cli.command {
            Commands::Up { memory, cpus, .. } => Ok((memory, cpus)),
            _ => unreachable!(),
        }
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
