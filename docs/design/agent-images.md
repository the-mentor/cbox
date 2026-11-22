# Agent images (draft)

Status: **draft, not accepted.** The files are in `images/`. CI builds, smoke-tests and
publishes them (see "CI and publishing"), but `cbox` and the `justfile` do not use them, and
`base/`, `base-rust/`, `custom/`, the `justfile` and `cbox` still build and boot the current
images. Once this design is accepted, `docs/design/images.md`, `docs/design/general.md` and
`AGENTS.md` need updating, since they describe the current two layers.

## Intent

Today `base/` (`cbox-base`) bakes in Claude Code, and `custom/` adds Claude settings, plugins
and cbox/gateway wiring on top. We want to run other coding agents too (Codex, opencode, and
more later). So the default base should hold no agent. Each agent gets a thin layer with that
agent's default setup, and a separate customize image, built `FROM` any agent image, overrides
those defaults.

```
base            OS, shell, git, runtimes, the non-root user, common tooling. NO agent.
  |- claude     FROM base + Claude Code + its default settings/wrapper
  |- codex      FROM base + Codex CLI + default config
  |- opencode   FROM base + opencode + default config
        \_ customize image: FROM <any agent image> (ARG BASE_IMAGE) + overrides
```

## Goals

- A base image with no agent in it, the same for every agent.
- One small layer per agent: the CLI at a pinned version, plus defaults that make sense in any
  cbox box.
- Overrides live in a separate customize image, so personal or cbox-specific choices never
  sit in the agent layers.
- Agents can be built locally with one command.

## Non-goals (in this PR)

- Switching anything that boots a box to the published images (see the migration plan).
- Switching `just build`, `cbox up` or the default image over to these images.
- Changing or removing `base/`, `base-rust/` or `custom/`.
- A customize image in the repo. The README shows only a minimal example.
- A Rust variant (`base-rust/` today).

## Layout

`images/Dockerfile` has four stages: `base`, `claude`, `codex` and `opencode`. The global
`ARG BASE_IMAGE` (default `node:26-trixie-slim`, the same image `base/` uses) picks the OS image
the `base` stage starts from. A customize image is its own Dockerfile with its own
`ARG BASE_IMAGE=cbox-agent-<agent>:dev` / `FROM ${BASE_IMAGE}`, the same pattern
`custom/Dockerfile` uses today. Nothing is pinned by digest yet. A comment marks where the tag or
digest goes.

`images/docker-bake.hcl` builds `base` (`cbox-agent-base:dev`) and a matrix target,
`agent-${a}` for `a` in `claude`, `codex`, `opencode`, each with `target = a` and tag
`cbox-agent-${a}:dev`. The groups are `agents` (the three agents) and `default` (base plus
agents). The output is the default, loaded into the local docker, with no push.

### What went where

| Piece today | Goes to | Why |
| --- | --- | --- |
| `base/`: apt packages, `gh`, `uv`, `pre-commit`, the `sandbox-user` rename, `/workspace` | `base` (`yq` and `procps` are new in `images/`; `yq` is from apt, the Python jq-wrapper, v3.4.3 on trixie; Codex needs `ps` from `procps`) | Not agent-specific. |
| `base/`: Node (the `node:` base image) | `base` | Codex, opencode and Claude Code all install with npm here. It is also a general runtime. |
| `base/`: oh-my-posh and `POSH_THEMES_PATH` | `base` (ambiguous) | A shell tool. Today only the Claude status line uses it. See open questions. |
| `base/bashrc.d/boxlite.sh` (bash-completion, `lg` alias) | `base` (copied to `images/base/`) | Nothing Claude-specific in it. |
| `base/`: `npm install -g @anthropic-ai/claude-code` | `claude`, pinned with `CLAUDE_CODE_VERSION` | Agent. |
| `base/claude-wrapper.sh`, `claude-real`, `cc` link | `claude` (copied to `images/claude/`) | It handles only `ANTHROPIC_API_KEY`. `cc` no longer shadows the C compiler in `base`, only in `claude`. |
| `custom/claude.json`: `hasCompletedOnboarding`, `theme`, `/workspace` trust | `claude` defaults (`images/claude/claude.json`) | Any headless, ephemeral box needs them so Claude Code does not stop at first-run prompts. |
| `custom/claude.json`: `mcpServers.agentgateway` | `claude` defaults, and the same entry in the `codex` and `opencode` defaults | Every cbox box reaches the host gateway at `host.boxlite.internal:15003`, whichever agent runs in it. |
| `custom/settings.json`: `env.DISABLE_AUTOUPDATER` | `claude` defaults (`images/claude/settings.json`) | The image pins the version, and a self-update in an ephemeral box is lost on the next boot anyway. |
| `custom/settings.json`: `statusLine` (oh-my-posh + `mojada-claude.omp.json`) | `claude` defaults (`images/claude/`, theme file copied) | A default status line. A customize image overrides it (see below). |
| `custom/settings.json`: `attribution` (empty) | `claude` defaults | A default, overridable in a customize image. |
| `custom/settings.json`: `permissions.defaultMode: auto` | removed | Dropped from `custom/` in this PR. Nothing else in the repo sets a permission mode. |
| `custom/settings.json`: `hooks` → `cbox-hook`, `custom/cbox-hook.sh` | `claude` defaults (`images/claude/`, copied) | cbox's iTerm2 forwarding on the host. It does nothing outside iTerm2, and `CLAUDE_ITERM2_INTEGRATION=0` turns it off. Hooks merge across scopes, so a customize image can add hooks but not remove these. |
| `custom/mojada-claude.omp.json` | `claude` defaults (copied to `~/mojada-claude.omp.json`) | Goes with `statusLine`. |
| `custom/`: `awscli`, plugin marketplaces/plugins, private plugins | customize | Per person, per repo. |
| Codex `check_for_update_on_startup = false`, `/workspace` `trust_level = "trusted"` | `codex` defaults (`images/codex/config.toml`) | Same reasons as `DISABLE_AUTOUPDATER` and the Claude trust entry. |
| opencode `~/.config/opencode/opencode.json` `{"update": "disable"}` | `opencode` defaults | Same reason as `DISABLE_AUTOUPDATER`. The opencode docs say `update` is read only from the global config. |

### Install sources

| Agent | Method | Pin | Source |
| --- | --- | --- | --- |
| Claude Code | `npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}` | `CLAUDE_CODE_VERSION` in `images/Dockerfile` | https://code.claude.com/docs/en/setup#install-with-npm |
| Codex | `npm install -g @openai/codex@${CODEX_VERSION}` | `CODEX_VERSION` in `images/Dockerfile` | https://github.com/openai/codex (README: npm, a `curl` install script, or Homebrew) |
| opencode | `npm install -g @opencode/cli@${OPENCODE_VERSION}` | `OPENCODE_VERSION` in `images/Dockerfile` | https://opencode.ai/docs/ (npm, a `curl` install script, Homebrew, and others) |

npm was picked for all three because Node is already in `base`, `ARG` pins the version, and
nothing is piped from `curl` into `sh`. The pins were the npm registry's `latest` on the day this
was written. Each pin has a `# renovate:` comment (npm datasource), as `base/` does for uv and
pre-commit. The node image tag in `BASE_IMAGE` has none, same as `base/`.

## Defaults and overrides

Every layer writes plain files, and **nothing merges them at build time**. A later `COPY` to the
same path replaces the whole file. A customize image that copies its own `settings.json` must
therefore repeat any default it wants to keep. Where the agent CLI can merge for you, use the CLI
in a `RUN` instead of `COPY`. For example, `claude mcp add --scope user ...` or
`claude plugin install --scope user ...` change `~/.claude.json` or `~/.claude/settings.json` in
place, which is what `custom/` already does for plugins.

At runtime, each agent merges its own config scopes:

- **Claude Code.** Precedence, highest first: managed > `--settings` (command line) > project
  local (`.claude/settings.local.json`) > project (`.claude/settings.json`) > user
  (`~/.claude/settings.json`). The user scope is the lowest, so image defaults go there and
  anything a project or the command line sets wins. They do **not** go in managed settings
  (`/etc/claude-code/managed-settings.json`), because nothing can override managed settings.
  Scalar keys override, but list keys such as `permissions.allow` merge across scopes. An
  override can add entries to a list but cannot remove the image's entries. Defaults should
  therefore not put anything in `permissions.allow`/`deny` that a user might want gone.
- **Codex.** Precedence, highest first: CLI flags / `--config` > project `.codex/config.toml`
  (trusted projects only) > `--profile` file > user `~/.codex/config.toml` > cloud-managed >
  system `/etc/codex/config.toml` > built-in defaults
  (https://learn.chatgpt.com/docs/config-file/config-basic). Image defaults go in the user file
  `~/.codex/config.toml`, like the other agents (see Decisions). It outranks the system file, so
  a customize image overrides it by replacing the file.
- **opencode.** Lowest to highest: global `~/.config/opencode/opencode.json`, then project files,
  then system/managed (`/etc/opencode/opencode.json`), which wins. Keys that do not conflict
  are kept (https://opencode.ai/docs/config). As with Claude Code, image defaults go in the
  global file, never in the managed one.

### Overriding the claude defaults

A customize image should merge its overrides into `~/.claude/settings.json` with `jq` (in `base`),
not replace the file, so it only states what it changes:

```dockerfile
COPY overrides.json /tmp/overrides.json
RUN f=$HOME/.claude/settings.json \
    && jq -s '.[0] * .[1]' "$f" /tmp/overrides.json > /tmp/merged.json \
    && mv /tmp/merged.json "$f" && rm /tmp/overrides.json
```

`jq`'s `*` merges objects recursively and **replaces arrays**, so overriding `hooks` or
`permissions.allow` replaces that whole list. To drop a default key, replace the file (`COPY`)
instead of merging.

## CI and publishing

`.github/workflows/agent-images.yml` builds `images/` and is modeled on `base-image.yml`
(`docs/design/images.md` has the details the two share: tag rules, the weekly refresh, native
runners, the merge step that refuses a tag missing an architecture).

- **Images.** `ghcr.io/the-mentor/cbox-agent-base`, `cbox-agent-claude`, `cbox-agent-codex` and
  `cbox-agent-opencode`. Each is one `target` of `images/Dockerfile`, built by `image.yml`
  (native amd64 and arm64 runners, push by digest, then one merge). `image.yml` got two inputs
  for this: `target`, and `key`, which names the digest artifacts and cache scope so several
  images can come from one directory.
- **Why `image.yml` and not the bake action.** It already has the digest-and-merge step that
  guards the tags; the bake action has no merge step. The cost is that each image rebuilds the
  shared `base` stage itself, so the four images do not share layers in the registry. If that
  matters, one job per architecture running `docker/bake-action` over the bake file would build
  the base stage once, at the price of writing the digest collection and merge by hand.
- **Smoke test.** A `smoke` job builds the four images for amd64 into the runner's docker
  (so it also runs for fork PRs, which push nothing) and runs `images/smoke-test.sh`.
- **Callers.** `ci.yml` runs it on pull requests when the new `images` filter matches
  (`images/`, `agent-images.yml`, `image.yml` or `ci.yml`), tagging `pr-<n>` and pushing only
  for same-repo, non-Dependabot PRs; `CI` now needs it. `release.yml` runs it after a release
  with `<release tag>` and `latest`, built from the released commit with the cache skipped.
  A weekly schedule (Monday 06:47 UTC) and `workflow_dispatch` refresh `latest` and a dated tag.
- **One-time setup.** GHCR creates each new package private on its first push. After the first
  run, make the four packages public in their package settings, as for `cbox-base`.
- **Not covered.** The smoke test checks the images, not a booted box. Cleaning up old tags.

## Build options considered

1. **One multi-stage Dockerfile, `--target` per image.** (Chosen.) The base and the agent
   layers are in one file, so a change to `base` rebuilds every agent in one BuildKit graph,
   with no tag handoff between builds. Any single image builds with plain
   `docker buildx build --target <name>`.
2. **Bake with a `matrix` target.** (Chosen, on top of 1.) One command builds every agent,
   and BuildKit builds the shared `base` stage once. Adding an agent means adding a stage and
   a name in the matrix list. CI does not use Bake to build (see "CI and publishing"); the
   bake file is for local builds and the smoke job.
3. **Separate Dockerfiles per agent, with Bake `contexts = { base = "target:base" }`.** Not
   chosen, for now. It keeps files small and lets an agent's Dockerfile live on its own, but
   with three thin layers it is more files and more indirection for the same graph. It is still
   the obvious move if one agent's layer grows large or needs its own build context.

## Decisions

- **One user and home layout for all agents.** Decided: every agent shares the single
  `sandbox-user` (uid 1000, `/home/sandbox-user`, `/workspace`). `cbox` assumes these paths
  today (for example, `exec` runs `claude --continue`).
- **One customize image per agent.** Decided. Most overrides are agent config (settings, MCP
  entries, hooks), which does not carry across agents. Parts shared by all agents (apt
  packages, dotfiles) can still use a single `ARG BASE_IMAGE` Dockerfile.
- **Node and oh-my-posh stay in `base`.** Decided. All three agents install with npm, and
  oh-my-posh is a small shell tool. Revisit if a non-npm agent shows up or the Claude status
  line stays the only user.
- **`permissions.defaultMode: auto` is removed** from `custom/settings.json`. The default
  permission mode applies.
- **The `cc` link stays in `claude`.** Decided: same as `base/`. It still shadows the C
  compiler, which matters for a future Rust variant (see `base-rust/`'s `CC=gcc`); dropping it
  remains the cleaner option if that bites.

- **Agent config lives in the sandbox user's home.** Decided: each agent's defaults are plain
  files under `/home/sandbox-user` (`~/.claude.json`, `~/.claude/settings.json`,
  `~/.config/opencode/opencode.json`, `~/.codex/config.toml`), baked into the image. Not mounted
  at `cbox up`, not env vars, and not system or managed paths. Codex's file sets only the update check and
  `/workspace` trust. `approval_policy` and `sandbox_mode` are left at Codex's defaults, since
  they are risk choices for a customize image.

- **agentgateway is a default MCP server for every agent.** Decided: the entry for
  `http://host.boxlite.internal:15003/mcp` is baked into each agent's home config. An image
  used outside cbox gets a dead entry, which a customize image can replace.

## Not done / open questions

- **How `cbox up` / `just build` switch over.** Not done. The image names, the
  `-- <cmd>`/`exec` default command per agent (`claude` vs `codex` vs `opencode`), and a
  profile `image` field are all candidates. `base/` and `custom/` are deliberately left
  untouched here.
- **Migration from `base/` + `custom/`:** (1) accept this design (CI for `images/` exists);
  (2) publish `cbox-agent-claude` (first release or a manual run of `agent-images.yml`); (3) rebase `custom/` onto it (`CBOX_BASE_IMAGE`), moving its
  defaults out (onboarding, trust and the auto-updater are already in `claude`); (4) port
  `base-rust/` onto `base`; (5) remove `base/`, update `docs/design/images.md`,
  `docs/design/general.md` and `AGENTS.md`.
- **File duplication.** `images/base/bashrc.d/boxlite.sh`, `images/claude/claude-wrapper.sh` and
  `images/claude/cbox-hook.sh` are copies of files in `base/` (and `custom/` for the hook),
  because the build context is `images/`. `base/` and `custom/` stay in use until they are
  removed, so the copies can drift until then.

### Verified

Checked on amd64 with Docker Desktop (default builder), from the repo root:

- `docker buildx bake -f images/docker-bake.hcl --print` resolves `context = "images"` and
  prints all four targets and both groups.
- `docker buildx build --check` passes for `base`, `claude`, `codex` and `opencode`.
- All four images build (`bake base`, then `bake agents`).
- `base`: user `sandbox-user`, `/workspace`, and `jq`, `yq` (apt, 3.4.3), `gh`, `uv`,
  `pre-commit` and `oh-my-posh` run. There is no agent in it.
- The agent CLIs run at the pinned versions, so `npm install -g @opencode/cli` works as root
  on amd64 and its binary resolves.
- The baked config is in the home directory: the claude JSON files parse, the theme file and
  `cbox-hook` are installed, and the codex and opencode files carry the agentgateway entry.
- `cbox-hook` run with `TERM_PROGRAM=iTerm.app` prints the `terminalSequence` line.
- Boxes booted with `cbox up -i cbox-agent-<name>` (images pushed to the local registry):
  the host gateway answers from inside the box (HTTP 406 to a plain `curl`), `claude mcp list`
  and `opencode mcp list` show `agentgateway` connected, and `codex mcp list` lists it as
  enabled. Codex runs. opencode logs `update check skipped reason=policy-disabled`, so its
  baked `update` setting is read. opencode 2.x lists MCP servers only after its first
  request in a directory loads the config, so the first `mcp list` can be empty.
- Codex needs `ps` (the `procps` package, added to `base`) to start its background server.
- CI on PR 160: `agent-images.yml` built base, claude, codex and opencode for amd64 and arm64
  on native runners, the merge step found both architectures for each, the `smoke` job passed,
  and `CI` was green with `base-image` and `custom-image`.
- `opencode` is large (about 2.3GB on disk, 708MB of content, against about 380MB for the
  other agents).

### Unverified

- arm64: the images build on a native arm64 runner and the merge finds both architectures, but
  nothing runs an arm64 image yet (the smoke test is amd64 only), so the opencode platform binary
  is untested there.
- That codex loads its baked update-check and trust settings (its MCP entry is read).
- That the status line and the hook events reach iTerm2, and that the claude first run shows
  no theme or trust prompt (run in a real box with `cbox up -i`).
- That the image works for `cbox up` and exec, and that codex runs without further setup.
- The codex and opencode docs list a `curl` script and npm side by side, and neither says
  which one it prefers. npm was a choice, not a documented recommendation.
