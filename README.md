# cbox

[![CI](https://github.com/the-mentor/cbox/actions/workflows/ci.yml/badge.svg)](https://github.com/the-mentor/cbox/actions/workflows/ci.yml)
[![base-image](https://github.com/the-mentor/cbox/actions/workflows/base-image.yml/badge.svg)](https://github.com/the-mentor/cbox/actions/workflows/base-image.yml)
[![Release](https://img.shields.io/github/v/release/the-mentor/cbox)](https://github.com/the-mentor/cbox/releases/latest)
[![License: MIT](https://img.shields.io/github/license/the-mentor/cbox)](LICENSE)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/the-mentor/cbox)

Build and run [Claude Code](https://github.com/anthropics/claude-code) inside a
[BoxLite](https://boxliteai.com) microVM, with an MCP config baked in that points Claude
Code at a host-side [agentgateway](https://agentgateway.dev). One `just` command builds the
image and boots the box.

Boxes are driven by `cbox`, a small Rust binary in `cbox/` that embeds BoxLite as a library
(BoxLite's runtime is compiled into it), so there is no separate `boxlite` CLI to install. The
`justfile` is the front door: Docker-side recipes (building images, the local registry, the
gateway) run directly, and `up`/`exec`/`down`/`list` forward to `cbox`.

This repo covers both halves: the **box side** (building the image, running the VM) and the
**host side** (`agentgateway/`, run with `just gateway-up`). The box's baked MCP config points
at `http://host.boxlite.internal:15003/mcp`, which the gateway serves. The gateway can also
broker Anthropic traffic — with an API key it holds the key host-side so the VM never sees it.

## How it works

- **Two-layer image.** `base/` builds `cbox-base` (Debian trixie + Node 26 + Claude Code,
  plus `gh`, `uv`, `pre-commit`, `lazygit` and oh-my-posh), running as a non-root
  `sandbox-user` with passwordless `sudo`. CI publishes it to
  `ghcr.io/the-mentor/cbox-base` on every release and weekly, so you normally never build it
  yourself. `custom/` layers `cbox-custom` on top, baking `custom/claude.json` in as
  `/home/sandbox-user/.claude.json` (theme, onboarding, and a user-scoped `agentgateway` MCP
  server) and installing the plugins listed in `custom/Dockerfile`. Nothing is baked into
  `/workspace`, so mounting a host directory there clobbers no config. See
  `docs/design/images.md`.
- **Image handoff via a local registry.** BoxLite does not read Docker's local image
  store, so the custom image is pushed to a local `registry:2` (managed by docker compose
  under `local-development/registry/`) and `cbox` pulls it from there. `cbox` is actually
  pointed at `registries.local.json` (gitignored), auto-created from the tracked
  `registries.json` template the first time you run `just up`/`up-dev`, so it's safe to add
  authenticated registries (e.g. ECR via `just registry-login`, see below) locally without
  ever touching the tracked file.
- **Credentials.** Secrets are read from a gitignored `.env` and never baked into an image.
  They reach the box two different ways, and the difference matters. Most are **forwarded** as
  ordinary environment variables when set: Claude auth is one mutually exclusive set chosen by
  what `.env` contains — subscription, direct API key, or gateway-keyed API key (see **The
  host-side gateway** below for which vars each set includes) — plus `ANTHROPIC_MODEL`
  optionally on top, and git identity (`GIT_AUTHOR_*` / `GIT_COMMITTER_*`). Unset vars are
  skipped. GitHub credentials are **not** forwarded: `GH_TOKEN`/`GITHUB_TOKEN` become
  BoxLite *secrets*, so the box sees only a placeholder and a host-side proxy substitutes the
  real value into GitHub-bound HTTPS. Both paths are implemented by `cbox` (see
  `docs/design/cbox.md`), which the `justfile` recipes forward to.
- **GitHub.** Setting `GH_TOKEN` (or `GITHUB_TOKEN`) authenticates the `gh` CLI
  automatically, and `git clone`/`push` over HTTPS work too — but not via gh's credential
  helper, which the box no longer uses. Git authenticates through an `http.extraHeader`
  carrying a placeholder that the host-side proxy substitutes, because git builds Basic auth
  itself and base64-encoding would hide a placeholder from the proxy. `docs/design/cbox.md`
  explains why that forces two separate secrets. Commit identity still comes from the
  `GIT_AUTHOR_*` / `GIT_COMMITTER_*` vars.

## Prerequisites

- An Apple Silicon Mac, or Linux x86_64 with KVM (`/dev/kvm`) — what BoxLite's microVMs need.
  Prebuilt `cbox` binaries exist for exactly these two; anything else has to build from source.
- [`docker`](https://docs.docker.com/get-docker/) with `docker compose`, for building the image,
  the local registry and the gateway
- [`just`](https://github.com/casey/just)
- `sqlite3` (optional) — used by `just clean-cache` to refresh BoxLite's image cache after a
  rebuild; without it the step is skipped with a warning
- To build `cbox` from source instead of downloading it: a Rust toolchain and `protoc >= 3.12`
  (`brew install protobuf` / `apt install protobuf-compiler`)

## Setup

Get the `cbox` binary. Either download a prebuilt one from the
[latest release](https://github.com/the-mentor/cbox/releases/latest), or compile it:

```bash
just install-cbox           # download the latest release's binary for this platform
just install-cbox v0.1.12   # pin a specific release
just build-cbox             # or compile it from source (cargo build --release)
just version                # check which cbox is installed
```

Both put it at `cbox/target/release/cbox`, the path the `up`/`exec`/`down`/`list` recipes run;
they fail with a pointer to these two recipes if it's missing. `just up-dev` runs `build-cbox`
for you. Use `install-cbox` unless you're changing `cbox/` itself — a downloaded binary can't
reflect local edits.

Copy the env template and set your credentials:

```bash
cp .env.example .env
# Claude auth — pick ONE:
#   subscription: `claude setup-token`, then CLAUDE_CODE_OAUTH_TOKEN=...
#                 optionally ANTHROPIC_BASE_URL=http://host.boxlite.internal:15002/claude
#   API key:      ANTHROPIC_API_KEY=... plus
#                 ANTHROPIC_BASE_URL=http://host.boxlite.internal:15002/api
#                 and ANTHROPIC_AUTH_TOKEN=unused (value unchecked; the gateway
#                 attaches the real key, so it never enters the box)
# Optional GitHub: set GH_TOKEN=... (a PAT) — substituted into the box's GitHub traffic
#                  host-side (the box only ever holds a placeholder), and used directly by
#                  the gateway's github MCP target
# Optional git identity: GIT_AUTHOR_NAME / GIT_AUTHOR_EMAIL
```

If `ANTHROPIC_API_KEY` isn't actually an Anthropic key — a LiteLLM key, say — set
`AGENTGATEWAY_ANTHROPIC_UPSTREAM_HOST` in `.env` to the bare `host:port` it should be sent to
instead (no scheme, no path). That changes only where the `/api` route forwards to; it is
unrelated to `ANTHROPIC_BASE_URL`, which is where the box sends traffic — always the gateway,
in this mode.

> **Upgrading:** if your `.env` already sets `ANTHROPIC_BASE_URL` for some other proxy, add
> `ANTHROPIC_AUTH_TOKEN=...` to it. Setting `ANTHROPIC_BASE_URL` now means "the credential
> lives outside the box", so `ANTHROPIC_API_KEY` is no longer forwarded — without an auth
> token the box would reach your proxy with no credential at all.

Copy the registries template the same way (or let `just up`/`up-dev` create it for you on first
run):

```bash
cp registries.json registries.local.json
```

`registries.local.json` is gitignored — it's the file BoxLite's `--config` actually reads, so
it's where `just registry-login` (see below) writes credentials for authenticated registries
like ECR, without ever touching the tracked `registries.json`.

## Usage

```bash
just up-dev            # build custom on the published base, build cbox, then boot the box
just up                # boot the box without rebuilding (image and cbox must already exist)
just build             # start the local registry, build custom on the published base, push custom
just build-local       # build base/ locally too, for changing base/ itself
just exec              # open a session in the running box (alias: just shell)
just list              # list running boxes (-a includes stopped ones)
just down              # stop and remove the box
just clean-cache       # make BoxLite re-pull a rebuilt image (build runs it for you)
just gateway-up                  # start the host-side agentgateway (MCP + Anthropic routes)
just gateway-down                # stop it
just gateway-logs                # follow its logs
just gateway-generate-ui-password # change the admin UI's default credentials, see below
just ci-local          # run CI's Linux build job locally via nektos/act
```

Use `just up-dev` the first time (or after changing the image); use `just up` for a fast
boot once the image is built. Both run Claude Code interactively inside the box, so `.env`
needs one of the Claude auth sets above. `-- <cmd>` launches something else instead (e.g.
`just up -- bash`).

`build`, `build-image`, `build-base`, and `build-local` forward any extra arguments to `docker build`. `build`
and `build-image` build only `custom/`, on top of `CBOX_BASE_IMAGE` (the published
`ghcr.io/the-mentor/cbox-base:latest` by default — see `docs/design/images.md`):

```bash
just build --no-cache                  # rebuild custom from scratch, on a freshly pulled base
just build-base --no-cache             # rebuild base/ only, locally, ignoring the layer cache
just build-local --no-cache            # rebuild base + custom locally, ignoring the layer cache
```

Reach for `--no-cache` when a build step whose command text never changes has gone stale —
Docker keeps serving the cached layer for `npm install -g @anthropic-ai/claude-code` or the
oh-my-posh `curl | sh` installer, so a plain `just build` will not pick up newer versions of
either. In practice this rarely matters locally: `cbox-base` is rebuilt weekly by CI and
published, so a plain `just build` already picks up the newer Claude Code from the refreshed
`:latest`. `just build-local --no-cache` gets the same result without depending on the published
image. Claude Code's in-box auto-updater is off either way (`DISABLE_AUTOUPDATER` in
`custom/settings.json`), since the npm global prefix is root-owned and the box's disk would lose
the update on the next `-f` anyway.

The `agentgateway` MCP server is configured user-scoped in `/home/sandbox-user/.claude.json`, so Claude
Code points at the host gateway in any project — including a mounted host directory.

`up`, `up-dev`, `exec`/`shell`, and `down` take an optional box name (default: derived from the
enclosing git repo's root directory, falling back to the cwd's name; pin one with `CBOX_NAME`), so you
can run several boxes side by side. `up`/`up-dev` also accept `-f`/`--force` to replace an existing box of the same name with a
fresh one (without it, a name collision resumes the existing box instead — see below):

```bash
just up-dev my-box     # build + boot a box named "my-box"
just up my-box -f      # re-boot it from scratch, replacing whatever was there
just up --cwd          # boot with the host current directory mounted at /workspace
just shell my-box      # open a session in it
just down my-box       # tear it down
```

Closing the terminal (or losing it to a crash) stops the box rather than removing it — the
disk and box record survive, and running `just up` again against the same name resumes it
(cold-booting the VM again, not a suspend/resume) instead of erroring on the collision. That
resumed box keeps the credentials, mounts, disk size, memory and CPUs it had when first created, so `cbox`
warns on resume and specifically calls out any secret whose value has changed since (e.g. a
rotated token) — `-f` is how to pick up today's settings instead. Pass `-d`/`--detach` to keep
the box running after the terminal closes, so `just exec` can reach it later. `just exec` also
works while `just up` is still attached — it opens a second session in the same box — and
against a stopped box it starts it first.

`up`/`up-dev` also accept `-c`/`--cwd` (mount the host current directory onto `/workspace`),
`-v host:box` (mount an arbitrary host folder, repeatable), `-e KEY=VALUE` (inject an
extra environment variable into the box, repeatable), `-i`/`--image` (boot a different
image path instead of the locally built `cbox-custom`), and `--disk-size <GB>`,
`--memory <GB>` and `--cpus <N>` (defaults 10 GB, 4 GiB and 2 — set when a box is created, so
use `-f` to change them on an existing one):

```bash
just up -e test=1 -e test2=2   # boot with test=1 and test2=2 set in the box
just up -i localhost:5551/library/cbox-custom:v2   # boot a specific tag
just up -f --memory 8 --cpus 4 # recreate the box with more resources
```

Other recipes: `just registry-up` / `just registry-down` manage the local registry
directly; `just gateway-up` / `just gateway-down` / `just gateway-logs` manage the host-side
agentgateway (see below); `just --list` shows everything.

### iTerm2 integration

iTerm2's own Claude Code integration (the tab status, dot and detail line) is a Claude Code
hook, `~/.config/iterm2/cc-status`, that iTerm2 installs on your Mac. It is a macOS binary
driving iTerm2 through its API socket, so it can't run in the box. Instead the image bakes
hooks (`custom/settings.json`) that run `custom/cbox-hook.sh` for every event. The script
returns the event to Claude Code as a hook `terminalSequence` (an `OSC 777;cbox-hook`
sequence), so it travels out through the terminal stream. `cbox up`/`exec` strips those
sequences out and pipes each event into `cc-status` on the host, so the status shows up in
the iTerm2 tab you ran `cbox` from.

In any other terminal the in-box hook exits straight away, so nothing is sent. Nothing to
configure: `cbox` uses `~/.config/iterm2/cc-status` when it exists. Set
`CBOX_HOOK_COMMAND` to use another host command, or set it to an empty string to turn the
forwarding off. Boot with `just up -e CLAUDE_ITERM2_INTEGRATION=0` to stop one box from
emitting events at all.

### The host-side gateway

`just gateway-up` runs agentgateway from `agentgateway/docker-compose.yml`. Every port it
publishes binds `127.0.0.1` only, which keeps it off your LAN — but that is not the same as
keeping it off the box: `host.boxlite.internal` resolves to the host loopback proxy, so **any
running box can reach any port this compose file publishes on 127.0.0.1**, exactly as if it
were the host itself. Loopback narrows the audience to "this machine plus every box on it," not
to "the host only." That's why the admin API's port is not published by default — see below.

For gateway changes that should stay on your machine only (an extra MCP server, say),
create `agentgateway/docker-compose.override.yml`. It is gitignored, and when it exists
`just gateway-up`/`gateway-down`/`gateway-logs` pass it to compose after the base file, so
it can add services or override mounts. To change `config.yaml` too, copy it to
`agentgateway/config.local.yaml` (also gitignored) and mount that over `/config.yaml` from
the override. Anything it publishes is reachable from every box too.

| Bind | Serves |
|---|---|
| `:15003/mcp` | multiplexed MCP tools (`github` live, proxied to a sibling `github-mcp` container — not GitHub's remote endpoint; others commented in `agentgateway/config.yaml`) |
| `:15002/claude` | Anthropic passthrough — your subscription OAuth token goes upstream untouched |
| `:15002/api` | Anthropic-Messages-API keyed — the gateway attaches `ANTHROPIC_API_KEY`, which stays on the host; upstream defaults to `api.anthropic.com` but is configurable via `AGENTGATEWAY_ANTHROPIC_UPSTREAM_HOST` (e.g. for a LiteLLM key) |
| `:15001/ui` | raw admin API (agentgateway's built-in admin interface) — **not published by default** (commented out in `agentgateway/docker-compose.yml`); its `/config_dump` is unauthenticated and returns real credential values, so publishing it hands every box a way to read `ANTHROPIC_API_KEY` back out. Uncomment the port temporarily for local debugging only while no untrusted box is running |
| `:15000/ui` | admin UI — **on by default**; the same config viewer and tool playground as the admin API above, behind HTTP basic auth. See "Admin UI" below |
| `:16686` | Jaeger's trace-viewer UI — read-only, no auth. Traces from `frontendPolicies.tracing` are viewed here; agentgateway's own admin UI has no traces page. Reachable from every box, holds no credentials |

(`:15003` and `:15002` are two separately named gateways in `agentgateway/config.yaml`'s
`gateways:` map — `mcp-gateway` and `llm-gateway` — not one gateway with two binds; the
`:15000` admin UI is a third, `ui-gateway`. `:16686` belongs to `jaeger`, a plain sibling
container like `github-mcp`, not a fourth entry in that map.) `:15003` also allows CORS from the admin UI's
tool playground (`127.0.0.1:15000`) so it can call the MCP endpoint directly from browser
JavaScript; the box itself talks to it server-to-server and is unaffected either way.

The gateway is long-lived and restarts with Docker; `just up`/`up-dev` do not start it. If
`ANTHROPIC_BASE_URL` points at it and it is not running, the box will fail to reach Anthropic
— `just gateway-logs` is the first thing to check.

The `/mcp` half works in every auth mode. Only the keyed mode keeps a credential off the VM:
in subscription mode Claude Code must hold the OAuth token to send it, so that mode buys
observability and a single egress point, not credential custody.

**What actually stays off the box.** "The gateway keeps credentials host-side" is about one
credential, not all of them:

| Credential | Reaches the box? | Why |
|---|---|---|
| `ANTHROPIC_API_KEY` | No, when `ANTHROPIC_BASE_URL` points at the gateway's `/api` route | the box never calls Anthropic directly in that mode — the gateway does it on the box's behalf, so the key has no reason to be there. With no `ANTHROPIC_BASE_URL` set at all, `cbox`'s `env::llm_passthrough()` forwards this key straight into the box instead — the gateway isn't in the loop, so this guarantee only applies to gateway-keyed mode |
| `GH_TOKEN` / `GITHUB_TOKEN` | No | the box holds only a placeholder (`<BOXLITE_SECRET:gh>`); a host-side proxy substitutes the real token into requests to `github.com`/`api.github.com`, so `gh`, `git clone`, and `git push` all authenticate without the value ever entering the VM. See `docs/design/cbox.md` |

**Troubleshooting**

| Symptom | Cause |
|---|---|
| `/mcp` connects but lists no tools | `GH_TOKEN` unset or expired — the `github-mcp` container's GitHub API calls 401, visible in `just gateway-logs` |
| 401 from Anthropic | your `ANTHROPIC_BASE_URL` path and your credential disagree: `/claude` needs the OAuth token, `/api` needs the gateway to have `ANTHROPIC_API_KEY` |
| 401 in subscription mode with the right path | `ANTHROPIC_AUTH_TOKEN` is set and shadowing the OAuth token — unset it |
| 400 `Extra inputs are not permitted` | beta headers the backend rejects; set `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS=1` and pass it with `-e`, or add it to `UNCONDITIONAL_PASSTHROUGH` in `cbox/src/env.rs` |
| Box can't reach Anthropic at all | the gateway isn't running — `just gateway-up` |

### Admin UI

The admin UI (config viewer + MCP tool playground) is **on by default**, on its own port
(`15000`), separate from the raw admin API (`15001`) covered above. `:15001` stays
unpublished because it's unauthenticated — `host.boxlite.internal` means any port published
in `agentgateway/docker-compose.yml` is reachable from every running box, not just the host,
and an unauthenticated UI would hand a sandboxed box the same live-config read. `:15000` is
safe to publish instead because it sits behind HTTP basic auth: a box without the credential
just gets a 401.

`just gateway-up` bootstraps that credential with no setup step: on first run it copies the
tracked template `agentgateway/htpasswd.default` to `agentgateway/htpasswd` (gitignored) only
if the latter doesn't already exist, so the UI comes up immediately at
`http://127.0.0.1:15000` with the **default login `admin` / `agentgateway`**.

**That default is a known, published credential.** Anything with network reach to `:15000` —
which, per the `host.boxlite.internal` note above, includes every box this repo boots — can
look it up and log in. It is accepted here because this repo's boxes typically do not mount
the repo itself, so a box has no way to read the password or the htpasswd hash off disk
another way; it is not a substitute for changing the password before relying on this port for
anything sensitive, or before publishing it more broadly than `127.0.0.1`.

Change it at any time with:

```bash
just gateway-generate-ui-password            # prompts for a new password for user "admin"
just gateway-generate-ui-password otheruser  # or a different username
```

This overwrites the live `agentgateway/htpasswd` only — the tracked
`agentgateway/htpasswd.default` template is never touched, so changing your password never
leaves a tracked file showing as modified. Then restart for it to take effect:

```bash
just gateway-down && just gateway-up
```

### Running from anywhere

`just` only finds a justfile in the current (or a parent) directory, so by default these
commands only work from inside this repo. To run them from any directory, install the
`cb` wrapper onto your `PATH`:

```bash
just install    # symlinks bin/cb into ~/bin (pass a dir to override)
cb up-dev       # now works from anywhere
```

`just uninstall` removes the symlink. The wrapper just runs `just --justfile
/path/to/this/repo/justfile "$@"`, so it behaves identically to running `just` from inside
the repo, including recipes' relative paths (e.g. `registries.json`). `-c`/`--cwd` uses
`just`'s `invocation_directory()` rather than `$PWD` so it mounts the directory you actually
ran the command from, not the repo's own directory.

`cbox` itself can also be run directly (`cbox up`, `cbox exec`, `cbox down`, `cbox list`, plus
`cbox name` to show which box name a directory resolves to). Outside `just`, nothing loads
`.env` or passes `--config` for you, so it reads credentials from `--env-file`,
`$CBOX_ENV_FILE`, or `~/.config/cbox/env` (never a `.env` in the current directory), and
registries from `--config`, `$CBOX_REGISTRIES`, or `~/.config/cbox/registries.json`. See
`docs/design/cbox.md`.

## Windows

Not supported natively: there is no Windows `cbox` binary, and BoxLite needs KVM. WSL2 with
nested virtualization (so `/dev/kvm` exists inside it) is the only route, using the Linux
instructions above; it is untested.
