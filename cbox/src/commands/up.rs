//! `cbox up` — create a box and attach the terminal to it.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use boxlite::{BoxCommand, BoxStatus, BoxliteOptions, BoxliteRuntime, LiteBox};

use crate::{attach, boxopts, config, env, envfile, naming, netdrift, netpolicy, secrets, sidecar};

pub struct UpArgs {
    pub name: Option<String>,
    pub force: bool,
    pub cwd_mount: bool,
    pub volumes: Vec<String>,
    pub env_flags: Vec<String>,
    pub image: String,
    pub config: Option<PathBuf>,
    pub secret_flags: Vec<String>,
    pub env_file: Option<PathBuf>,
    pub cmd: Vec<String>,
    pub disk_size_gb: Option<u64>,
    pub memory_gb: Option<u32>,
    pub cpus: Option<u8>,
    /// Let the box outlive this process. See `boxopts::build`'s `detach`
    /// comment for the full reasoning; default is `false`, matching what
    /// the pre-cbox justfile actually passed to `boxlite run`.
    pub detach: bool,
    /// `--allow-net` rules, unresolved.
    pub allow_net: Vec<String>,
    /// `--network disabled`.
    pub network_disabled: bool,
}

pub async fn run(args: UpArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("cannot read current directory")?;
    let resolved = naming::resolve(args.name.as_deref(), &cwd);
    let name = resolved.name;
    let network = netpolicy::resolve(&args.allow_net, args.network_disabled)?;

    // Load the env file, if any, before anything below reads the process
    // environment: passthrough selection, secret source lookup, and GitHub
    // token detection all need to see whatever it provides. A variable
    // already set in the real environment wins — this only fills gaps.
    let env_file_path = envfile::resolve_path(args.env_file.as_deref());
    if let Some(path) = &env_file_path {
        envfile::apply(path);
    }

    // Secrets first: their source variables must be withheld from passthrough.
    let mut specs = Vec::new();
    if secrets::has_github_token() {
        specs.push(secrets::github_spec());
    }
    for flag in &args.secret_flags {
        specs.push(secrets::parse_secret_flag(flag)?);
    }
    let built = secrets::build(&specs)?;
    let has_github = specs.iter().any(|s| s.name == "gh");

    // Computed before `built.secrets` is moved into `boxopts::build` below.
    // Never the values themselves -- see `sidecar::hash_secret_value` for
    // why a hash is enough and adequate here.
    let secret_hashes: std::collections::BTreeMap<String, u64> = built
        .secrets
        .iter()
        .map(|s| (s.name.clone(), sidecar::hash_secret_value(&s.value)))
        .collect();

    let passthrough = env::passthrough_vars();
    let mut plain = env::compose(&args.env_flags, &passthrough, &built.source_vars)?;
    env::add_gateway_placeholder(&mut plain);
    let anthropic_base_url =
        plain.iter().find(|(k, _)| k == "ANTHROPIC_BASE_URL").map(|(_, v)| v.clone());
    plain.extend(built.env.clone());
    plain.push((
        "TERM".into(),
        std::env::var("TERM").unwrap_or_else(|_| attach::DEFAULT_TERM.into()),
    ));
    plain.push(("BOX_NAME".into(), name.clone()));

    let home = config::box_home(&name);
    secure_box_home(&home)?;

    let registries = config::resolve_config_path(args.config.as_deref())
        .map(|p| config::load_registries(&p))
        .unwrap_or_default();

    let runtime = BoxliteRuntime::new(BoxliteOptions {
        home_dir: home.clone(),
        image_registries: registries,
    })
    .context("failed to open the BoxLite runtime")?;

    if args.force {
        let _ = runtime.remove(&name, true).await;
    }

    let cmd = if args.cmd.is_empty() { vec!["claude".into()] } else { args.cmd };

    // The one line that turns a multi-hour "why did Claude just exit"
    // diagnosis into something visible immediately: warn, don't fail — the
    // user launching a non-`claude` command, or one that authenticates some
    // other way, is not this function's business.
    if cmd.first().map(String::as_str) == Some("claude") && !env::any_anthropic_credential_set() {
        let looked = match &env_file_path {
            Some(loc) => format!("cbox looked for an env file at {}", loc.path.display()),
            None => "cbox could not determine an env file location (no $HOME)".to_string(),
        };
        eprintln!(
            "cbox: warning: no Anthropic credential found (checked ANTHROPIC_API_KEY, \
             ANTHROPIC_AUTH_TOKEN, ANTHROPIC_BASE_URL, CLAUDE_CODE_OAUTH_TOKEN). \
             Claude will not be able to authenticate. {looked} \
             (override with --env-file or $CBOX_ENV_FILE)."
        );
    }

    let flags = boxopts::UpFlags {
        image: args.image,
        cwd_mount: args.cwd_mount,
        volumes: args.volumes,
        cmd,
        invocation_dir: cwd,
        disk_size_gb: args.disk_size_gb,
        memory_gb: args.memory_gb,
        cpus: args.cpus,
        detach: args.detach,
        network,
    };
    let secret_hosts = netpolicy::secret_hosts(&built.secrets);
    let runs_claude = flags.cmd.first().map(String::as_str) == Some("claude");
    let options = boxopts::build(&flags, built.secrets, plain)?;

    println!("cbox: starting {name} ({})", resolved.source.describe());
    // get_or_create rather than create: with detach: false the common case
    // is exactly a name collision -- the box from a previous session is
    // sitting there Stopped, and that must be resumed, not rejected. `-f`
    // above already removed any existing box under this name, so on that
    // path this always creates fresh.
    let (mut litebox, mut created) = runtime
        .get_or_create(options.clone(), Some(name.clone()))
        .await
        .context("failed to create or reuse the box")?;

    // Only meaningful where the requested policy is actually applied (a new
    // or recreated box), so it's printed in those two arms.
    let direct_warning = netpolicy::direct_anthropic_warning(
        &flags.network,
        runs_claude,
        anthropic_base_url.as_deref(),
    );

    if created {
        for line in netpolicy::policy_lines(&flags.network, &secret_hosts, runs_claude) {
            eprintln!("{line}");
        }
        if let Some(w) = &direct_warning {
            eprintln!("{w}");
        }
    } else {
        // get_or_create ignores a reused box's options and BoxLite does not
        // compare network policy on reuse, so check it here: a requested
        // --allow-net must never be silently dropped (docs/design/allow-net.md).
        let info = litebox.info().await.context("failed to read the existing box's settings")?;
        let recorded = netpolicy::Recorded::from_info(info.network.as_ref());
        // Which secrets the box was created with (names only): BoxLite
        // doesn't expose a reused box's secret hosts, this is the closest.
        let existing = sidecar::read(&home);
        let box_secret_names: Vec<String> = existing
            .as_ref()
            .map(|e| e.secret_hashes.keys().cloned().collect())
            .unwrap_or_default();
        if netdrift::needs_prompt(&flags.network, &recorded) {
            let running = matches!(
                info.status,
                BoxStatus::Running | BoxStatus::Stopping | BoxStatus::Paused
            );
            let text = netdrift::prompt_text(&name, &recorded, &flags.network, running);
            let choice = tokio::task::spawn_blocking(move || {
                netdrift::choose(&mut netdrift::TtyPrompter, &text)
            })
            .await
            .context("the network-policy prompt failed")?;
            match choice {
                netdrift::Choice::Recreate => {
                    drop(litebox);
                    runtime
                        .remove(&name, true)
                        .await
                        .context("failed to remove the box to recreate it")?;
                    litebox = runtime
                        .create(options, Some(name.clone()))
                        .await
                        .context(
                            "the old box was removed but recreating it failed; \
                             rerun `cbox up` to create it",
                        )?;
                    created = true;
                    for line in netpolicy::policy_lines(&flags.network, &secret_hosts, runs_claude) {
                        eprintln!("{line}");
                    }
                    if let Some(w) = &direct_warning {
                        eprintln!("{w}");
                    }
                }
                netdrift::Choice::Continue => {
                    for line in continued_policy_lines(&recorded, &box_secret_names, runs_claude) {
                        eprintln!("{line}");
                    }
                }
                netdrift::Choice::Abort => {
                    bail!("aborted; box {name} left untouched");
                }
                netdrift::Choice::NoTerminal => {
                    bail!(
                        "box {name} was created with egress: {}; you asked for: {}. \
                         No terminal to confirm on; rerun with -f/--force to recreate it.",
                        recorded.describe(),
                        flags.network.describe()
                    );
                }
            }
        } else {
            for line in reused_policy_lines(&recorded, &box_secret_names, runs_claude) {
                eprintln!("{line}");
            }
        }
    }

    if created {
        // Best-effort, like the git bootstrap below: `cbox list` losing the
        // origin column for this one box is far better than `cbox up`
        // failing over a metadata write.
        if let Err(e) = sidecar::write(&home, &flags.invocation_dir, &secret_hashes) {
            eprintln!(
                "cbox: warning: could not record this box's origin ({e}); \
                 `cbox list` won't show a directory for it."
            );
        }
    } else {
        // `get_or_create`'s own doc: "the provided options are ignored (no
        // config drift validation)". So the reused box keeps whatever
        // credentials, mounts, disk size, memory and CPUs it had when first created,
        // silently -- unless this says so, that's invisible until something
        // fails (e.g. a rotated token 401ing), which is precisely the
        // failure class this whole project exists to prevent.
        // Network policy is checked separately above (netdrift).
        println!("{}", reuse_message(&name));
        if let Some(existing) = sidecar::read(&home) {
            let changed = sidecar::changed_secrets(&existing.secret_hashes, &secret_hashes);
            if !changed.is_empty() {
                // A warning, not a refusal: the whole point of reuse is to
                // resume a box that's otherwise fine, and most reuses won't
                // have rotated anything. Refusing here would force -f (a
                // full recreate) onto every rotation, including ones that
                // don't matter for this session (e.g. a secret this
                // invocation doesn't even use). Naming the stale secret and
                // the fix is what turns this from an invisible 401 later
                // into an actionable line now.
                eprintln!(
                    "cbox: warning: {} in your environment no longer match(es) what this box \
                     was created with -- it will keep substituting the OLD value(s) until you \
                     run with -f/--force to recreate it.",
                    changed.join(", ")
                );
            }
        }
    }

    // Idempotent on an already-Running box (the SDK's own doc on `start()`),
    // so this is correct whether the box above was just created, resumed
    // from Stopped, or was already Running.
    litebox.start().await.context("failed to start the box")?;

    // Only now, with the new box's disk created, can we tell which cached
    // disk images are still in use. An unchanged image is shared by the new
    // box and kept; one left over from an older image is not. Best-effort:
    // a failed sweep costs disk space, not the session.
    if args.force {
        match sweep_disk_images(&home) {
            Ok(freed) if !freed.is_empty() => {
                println!("cbox: deleted {} unused disk image(s)", freed.len())
            }
            Ok(_) => {}
            Err(e) => eprintln!("cbox: warning: could not sweep old disk images: {e:#}"),
        }
    }

    if has_github {
        run_git_bootstrap(&litebox).await;
    }

    let litebox = Arc::new(litebox);
    let home_for_socket = config::box_home(&name);
    let server = tokio::spawn({
        let litebox = Arc::clone(&litebox);
        let home = home_for_socket.clone();
        async move {
            if let Err(e) = crate::server::serve(litebox, home).await {
                eprintln!("cbox: control socket stopped: {e}");
            }
        }
    });

    let result = attach::attach(&litebox, &flags.cmd).await;

    // Clean shutdown unlinks the socket. A SIGKILL cannot, which is why the
    // client also handles a stale socket.
    server.abort();
    let _ = std::fs::remove_file(crate::server::socket_path(&home_for_socket));
    // `up`'s own exit status doesn't reflect the attached command's exit code
    // today (only `cbox exec` propagates that, via `commands/exec.rs`) — this
    // task didn't touch that, so keep dropping it here rather than changing
    // what `cbox up` reports to the shell.
    result.map(|_code| ())
}

/// Delete every `images/disk-images/*.ext4` in `home` that no qcow2 in the
/// home uses as its backing file, returning what was deleted.
///
/// BoxLite caches one ext4 per image digest and never collects them, so each
/// image rebuild leaves a ~2GB file behind. A box's disk (and any snapshot of
/// it) is a qcow2 whose header names its backing file by canonical path, so
/// scanning every qcow2 in the home finds every disk image still in use. Any
/// qcow2 that can't be read aborts the sweep rather than risk deleting a disk
/// image something depends on.
fn sweep_disk_images(home: &std::path::Path) -> Result<Vec<PathBuf>> {
    let dir = home.join("images/disk-images");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(vec![]) };

    let mut in_use = std::collections::HashSet::new();
    let mut stack = vec![home.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let e = e?;
            let path = e.path();
            // `images/` holds extracted layer rootfs trees: no box disks, but
            // absolute symlinks that would lead the walk onto the host's own
            // filesystem. file_type() doesn't follow symlinks for the rest.
            if e.file_type()?.is_dir() && path != home.join("images") {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "qcow2") {
                if let Some(backing) = qcow2_backing(&path)? {
                    in_use.insert(std::fs::canonicalize(&backing).unwrap_or(backing));
                }
            }
        }
    }

    let mut freed = vec![];
    for e in entries {
        let path = e?.path();
        if path.extension().is_none_or(|x| x != "ext4") {
            continue;
        }
        if !in_use.contains(&std::fs::canonicalize(&path)?) {
            std::fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
            freed.push(path);
        }
    }
    Ok(freed)
}

/// The backing file named in a qcow2 header, if any (qcow2 spec: magic at 0,
/// backing path offset as a u64 at 8, its length as a u32 at 16, big-endian).
fn qcow2_backing(path: &std::path::Path) -> Result<Option<PathBuf>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut h = [0u8; 20];
    f.read_exact(&mut h).with_context(|| format!("reading {}", path.display()))?;
    anyhow::ensure!(h[..4] == *b"QFI\xfb", "{} is not a qcow2 file", path.display());
    let offset = u64::from_be_bytes(h[8..16].try_into().unwrap());
    let len = u32::from_be_bytes(h[16..20].try_into().unwrap()) as usize;
    if offset == 0 || len == 0 {
        return Ok(None);
    }
    let mut buf = vec![0u8; len];
    f.seek(SeekFrom::Start(offset))?;
    f.read_exact(&mut buf).with_context(|| format!("reading {}", path.display()))?;
    Ok(Some(PathBuf::from(String::from_utf8(buf)?)))
}

/// Create the box's home directory if needed, and ensure it is owner-only.
///
/// The control socket `server::serve` binds inside this directory grants any
/// local process arbitrary TTY exec into a box that substitutes a real
/// credential (e.g. a GitHub token) into outbound HTTPS -- so this directory
/// must never be traversable by anyone but the owner. `create_dir_all` alone
/// yields 0755 and no-ops on an already-existing directory, so the
/// permission is set unconditionally afterward, which also tightens a home
/// left over-permissive by an earlier run. macOS does not reliably enforce a
/// Unix-domain socket's own mode on `connect()`, so this directory
/// permission -- not the socket's own mode, set separately in
/// `server::serve` -- is the one that actually gates access.
fn secure_box_home(home: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(home)
        .with_context(|| format!("cannot create box home {}", home.display()))?;
    std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("cannot restrict permissions on box home {}", home.display()))?;
    Ok(())
}

/// Point git at the pre-encoded secret so GitHub operations authenticate.
///
/// This step is entirely best-effort: it must never take `cbox up` down with
/// it. Two things can go wrong — the image might not have git at all, or the
/// bootstrap script itself might fail — and both are handled by warning to
/// stderr and moving on, never by propagating an error. A box that skipped
/// the bootstrap 401s on its first git operation against GitHub, which is
/// visible immediately and named by the warning below, and that outcome is
/// strictly better than killing the whole session over an inessential
/// configuration step.
async fn run_git_bootstrap(litebox: &LiteBox) {
    let has_git = match litebox
        .exec(BoxCommand::new("sh").args(["-lc", "command -v git >/dev/null 2>&1"]))
        .await
    {
        Ok(probe) => match probe.wait().await {
            Ok(result) => result.success(),
            Err(e) => {
                eprintln!(
                    "cbox: warning: could not check for git in the box ({e}); \
                     skipping GitHub credential bootstrap. git operations against \
                     GitHub will fail to authenticate until this is configured manually."
                );
                return;
            }
        },
        Err(e) => {
            eprintln!(
                "cbox: warning: could not check for git in the box ({e}); \
                 skipping GitHub credential bootstrap. git operations against \
                 GitHub will fail to authenticate until this is configured manually."
            );
            return;
        }
    };

    if !has_git {
        eprintln!(
            "cbox: warning: git not found in the box; skipping GitHub credential bootstrap. \
             git operations against GitHub will fail to authenticate."
        );
        return;
    }

    let script = secrets::git_bootstrap_script();
    match litebox.exec(BoxCommand::new("sh").args(["-lc", &script])).await {
        Ok(e) => match e.wait().await {
            Ok(result) if result.success() => {}
            Ok(result) => eprintln!(
                "cbox: warning: git credential bootstrap exited with code {}; \
                 git operations against GitHub will fail to authenticate.",
                result.exit_code
            ),
            Err(e) => eprintln!(
                "cbox: warning: git credential bootstrap failed ({e}); \
                 git operations against GitHub will fail to authenticate."
            ),
        },
        Err(e) => eprintln!(
            "cbox: warning: git credential bootstrap failed to start ({e}); \
             git operations against GitHub will fail to authenticate."
        ),
    }
}

/// Printed when `up` resumes an existing box: `get_or_create` ignores the
/// new options, so every setting below keeps its value from creation.
fn reuse_message(name: &str) -> String {
    format!(
        "cbox: reusing existing box {name}; its configuration (credentials, mounts, \
         disk size, memory, CPUs, network policy) dates from when it was first created. \
         Run with -f/--force to recreate it with today's settings instead."
    )
}

/// What a reused box's recorded policy looks like, when nothing was
/// requested: a restricted box must never look open just because the flags
/// were left off this time. BoxLite doesn't expose a reused box's secret
/// hosts, so `secret_names` (from the sidecar) stands in for them.
fn reused_policy_lines(
    recorded: &netpolicy::Recorded,
    secret_names: &[String],
    runs_claude: bool,
) -> Vec<String> {
    match recorded {
        netpolicy::Recorded::Known(p) => {
            let mut lines = netpolicy::policy_lines(p, &[], runs_claude);
            if matches!(p, netpolicy::Policy::Allow(_)) && !secret_names.is_empty() {
                lines.push(format!(
                    "cbox: also reachable on :443 via this box's secrets ({})",
                    secret_names.join(", ")
                ));
            }
            lines
        }
        netpolicy::Recorded::Unknown => {
            vec!["cbox: box network policy unknown (not recorded by BoxLite)".to_string()]
        }
    }
}

/// The `[c]` answer to the drift prompt: say so, then show what the box
/// actually enforces, same as a reuse without flags.
fn continued_policy_lines(
    recorded: &netpolicy::Recorded,
    secret_names: &[String],
    runs_claude: bool,
) -> Vec<String> {
    let mut lines =
        vec![format!("cbox: continuing with existing policy: {}", recorded.describe())];
    if matches!(recorded, netpolicy::Recorded::Known(_)) {
        lines.extend(reused_policy_lines(recorded, secret_names, runs_claude));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{
        continued_policy_lines, reuse_message, reused_policy_lines, secure_box_home,
        sweep_disk_images,
    };
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_reuse_message_names_memory_and_cpus_as_fixed_at_creation() {
        let msg = reuse_message("demo");
        assert!(msg.contains("memory"), "{msg}");
        assert!(msg.contains("CPUs"), "{msg}");
        assert!(msg.contains("-f/--force"), "{msg}");
    }

    #[test]
    fn the_reuse_message_names_the_network_policy_as_fixed_at_creation() {
        let msg = reuse_message("demo");
        assert!(msg.contains("network policy"), "{msg}");
    }

    #[test]
    fn reuse_without_flags_reports_the_recorded_policy() {
        use crate::netpolicy::{Policy, Recorded, resolve};
        assert!(reused_policy_lines(&Recorded::Known(Policy::Open), &[], true).is_empty());
        let lines = reused_policy_lines(&Recorded::Unknown, &[], true);
        assert_eq!(lines, ["cbox: box network policy unknown (not recorded by BoxLite)"]);

        let p = resolve(&["@npm".to_string()], false).unwrap();
        let lines = reused_policy_lines(&Recorded::Known(p), &[], true);
        assert!(lines[0].starts_with("cbox: egress restricted to: "), "{lines:?}");

        let lines = reused_policy_lines(&Recorded::Known(Policy::Disabled), &[], true);
        assert_eq!(lines[0], "cbox: network disabled");
    }

    #[test]
    fn a_reused_allow_list_box_names_its_secrets_but_other_policies_do_not() {
        use crate::netpolicy::{Policy, Recorded, resolve};
        let names = vec!["gh".to_string(), "npm".to_string()];
        let p = resolve(&["@npm".to_string()], false).unwrap();
        let lines = reused_policy_lines(&Recorded::Known(p.clone()), &names, true);
        assert_eq!(
            lines.last().unwrap(),
            "cbox: also reachable on :443 via this box's secrets (gh, npm)"
        );
        let lines = reused_policy_lines(&Recorded::Known(p), &[], true);
        assert!(!lines.iter().any(|l| l.contains("secrets")), "{lines:?}");
        for other in [Policy::Open, Policy::Disabled] {
            let lines = reused_policy_lines(&Recorded::Known(other), &names, true);
            assert!(!lines.iter().any(|l| l.contains("secrets")), "{lines:?}");
        }
    }

    #[test]
    fn continuing_prints_the_existing_policy_in_full() {
        use crate::netpolicy::{Recorded, resolve};
        let p = resolve(&["@npm".to_string()], false).unwrap();
        let names = vec!["gh".to_string()];
        let lines = continued_policy_lines(&Recorded::Known(p), &names, true);
        assert!(lines[0].starts_with("cbox: continuing with existing policy"), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("cbox: egress restricted to: ")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("the gateway IP opens every")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("via this box's secrets (gh)")), "{lines:?}");

        let lines = continued_policy_lines(&Recorded::Unknown, &names, true);
        assert_eq!(lines.len(), 1, "{lines:?}");
    }

    #[test]
    fn a_freshly_created_box_home_is_owner_only() {
        let dir = std::env::temp_dir()
            .join(format!("cbox-up-test-home-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        secure_box_home(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "fresh box home must be owner-only, got {mode:o}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_pre_existing_over_permissive_box_home_is_tightened() {
        // Guards against a home left over-permissive by an earlier run --
        // this is the case `create_dir_all` alone silently leaves open,
        // since it no-ops on an already-existing directory.
        let dir = std::env::temp_dir()
            .join(format!("cbox-up-test-home-loose-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        secure_box_home(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "a loosely-permissioned existing home must be tightened, got {mode:o}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sweep_keeps_disk_images_a_box_uses_and_deletes_the_rest() {
        let home = std::env::temp_dir().join(format!("cbox-up-test-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let images = home.join("images/disk-images");
        let disks = home.join("boxes/abc/disks");
        std::fs::create_dir_all(&images).unwrap();
        std::fs::create_dir_all(&disks).unwrap();
        let live = images.join("sha256-new-r1.ext4");
        let stale = images.join("sha256-old-r1.ext4");
        std::fs::write(&live, b"").unwrap();
        std::fs::write(&stale, b"").unwrap();

        // Minimal qcow2 header whose backing path is `live`, canonicalized
        // the way BoxLite writes it.
        let backing = std::fs::canonicalize(&live).unwrap();
        let backing = backing.to_str().unwrap().as_bytes();
        let mut qcow = vec![0u8; 64];
        qcow[..4].copy_from_slice(b"QFI\xfb");
        qcow[8..16].copy_from_slice(&64u64.to_be_bytes());
        qcow[16..20].copy_from_slice(&(backing.len() as u32).to_be_bytes());
        qcow.extend_from_slice(backing);
        std::fs::write(disks.join("disk.qcow2"), &qcow).unwrap();
        // A qcow2 without a backing file must not abort the sweep.
        let mut plain = vec![0u8; 64];
        plain[..4].copy_from_slice(b"QFI\xfb");
        std::fs::write(disks.join("guest-rootfs.qcow2"), &plain).unwrap();
        // Extracted layers are never walked: a non-qcow2 `.qcow2` there, or a
        // symlink out of the home, must not abort or escape the sweep.
        let layer = home.join("images/extracted/sha256-layer");
        std::fs::create_dir_all(&layer).unwrap();
        std::fs::write(layer.join("junk.qcow2"), b"not qcow2").unwrap();
        std::os::unix::fs::symlink("/", home.join("boxes/abc/root-link")).unwrap();

        let freed = sweep_disk_images(&home).unwrap();
        assert_eq!(freed, vec![stale.clone()]);
        assert!(live.exists());
        assert!(!stale.exists());

        std::fs::remove_dir_all(&home).unwrap();
    }
}
