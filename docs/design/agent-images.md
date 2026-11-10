# Agent images (draft)

Status: **draft, not accepted.** The files are in `images/`. CI does not build them, and
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

- CI builds or publishing. No workflow changes, no registry tags.
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
| `base/`: apt packages, `gh`, `uv`, `pre-commit`, the `sandbox-user` rename, `/workspace` | `base` | Not agent-specific. |
| `base/`: Node (the `node:` base image) | `base` | Codex, opencode and Claude Code all install with npm here. It is also a general runtime. |
| `base/`: oh-my-posh and `POSH_THEMES_PATH` | `base` (ambiguous) | A shell tool. Today only the Claude status line uses it. See open questions. |
| `base/bashrc.d/boxlite.sh` (bash-completion, `lg` alias) | `base` (copied to `images/base/`) | Nothing Claude-specific in it. |
| `base/`: `npm install -g @anthropic-ai/claude-code` | `claude`, pinned with `CLAUDE_CODE_VERSION` | Agent. |
| `base/claude-wrapper.sh`, `claude-real`, `cc` link | `claude` (copied to `images/claude/`) | It handles only `ANTHROPIC_API_KEY`. `cc` no longer shadows the C compiler in `base`, only in `claude`. |
| `custom/claude.json`: `hasCompletedOnboarding`, `theme`, `/workspace` trust | `claude` defaults (`images/claude/claude.json`) | Any headless, ephemeral box needs them so Claude Code does not stop at first-run prompts. |
| `custom/claude.json`: `mcpServers.agentgateway` | customize | cbox/gateway-specific: it points at `host.boxlite.internal:15003`. |
| `custom/settings.json`: `env.DISABLE_AUTOUPDATER` | `claude` defaults (`images/claude/settings.json`) | The image pins the version, and a self-update in an ephemeral box is lost on the next boot anyway. |
| `custom/settings.json`: `statusLine` (oh-my-posh + `mojada-claude.omp.json`) | customize | A personal theme. |
| `custom/settings.json`: `attribution` (empty) | customize | A personal or project policy. |
| `custom/settings.json`: `permissions.defaultMode: auto` | customize (ambiguous) | An opinion about risk, even in a sandbox. See open questions. |
| `custom/settings.json`: `hooks` → `cbox-hook`, `custom/cbox-hook.sh` | customize | They exist only for cbox's iTerm2 forwarding on the host. |
| `custom/mojada-claude.omp.json` | customize | Goes with `statusLine`. |
| `custom/`: `awscli`, plugin marketplaces/plugins, private plugins | customize | Per person, per repo. |
| Codex config | none yet | No default is clearly generic. See open questions. |
| opencode `~/.config/opencode/opencode.json` `{"update": "disable"}` | `opencode` defaults | Same reason as `DISABLE_AUTOUPDATER`. The opencode docs say `update` is read only from the global config. |

### Install sources

| Agent | Method | Pin | Source |
| --- | --- | --- | --- |
| Claude Code | `npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}` | `2.1.322` | https://code.claude.com/docs/en/setup#install-with-npm |
| Codex | `npm install -g @openai/codex@${CODEX_VERSION}` | `0.167.0` | https://github.com/openai/codex (README: npm, a `curl` install script, or Homebrew) |
| opencode | `npm install -g @opencode/cli@${OPENCODE_VERSION}` | `2.2.6` | https://opencode.ai/docs/ (npm, a `curl` install script, Homebrew, and others) |

npm was picked for all three because Node is already in `base`, `ARG` pins the version, and
nothing is piped from `curl` into `sh`. The pins were the npm registry's `latest` on the day this
was written. They have no `# renovate:` comment yet, so Renovate does not open PRs against a
draft.

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
  (https://learn.chatgpt.com/docs/config-file/config-basic). Image defaults could go in
  `/etc/codex/config.toml`, which ranks below the user file, so a customize image or the user
  can still override them.
- **opencode.** Lowest to highest: global `~/.config/opencode/opencode.json`, then project files,
  then system/managed (`/etc/opencode/opencode.json`), which wins. Keys that do not conflict
  are kept (https://opencode.ai/docs/config). As with Claude Code, image defaults go in the
  global file, never in the managed one.

## Build options considered

1. **One multi-stage Dockerfile, `--target` per image.** (Chosen.) The base and the agent
   layers are in one file, so a change to `base` rebuilds every agent in one BuildKit graph,
   with no tag handoff between builds. Any single image builds with plain
   `docker buildx build --target <name>`.
2. **Bake with a `matrix` target.** (Chosen, on top of 1.) One command builds every agent,
   and BuildKit builds the shared `base` stage once. Adding an agent means adding a stage and
   a name in the matrix list. Bake is also a natural fit for CI later, through
   `docker/bake-action`.
3. **Separate Dockerfiles per agent, with Bake `contexts = { base = "target:base" }`.** Not
   chosen, for now. It keeps files small and lets an agent's Dockerfile live on its own, but
   with three thin layers it is more files and more indirection for the same graph. It is still
   the obvious move if one agent's layer grows large or needs its own build context.

## Not done / open questions

- **One user and home layout for all agents?** The draft keeps the single `sandbox-user`
  (uid 1000, `/home/sandbox-user`, `/workspace`) for every agent. `cbox` assumes these paths
  today (for example, `exec` runs `claude --continue`).
- **Where each agent's default config lives:** baked into the image (the draft), mounted at
  `cbox up`, or passed as env vars. Codex has no default yet. The candidates (update checks,
  approval mode, sandbox mode) need checking against the Codex docs first, and the right file
  is probably `/etc/codex/config.toml`.
- **One customize image per agent, or one layer over any agent?** Most overrides are agent
  config, so a per-agent customize image looks likelier. A single Dockerfile with
  `ARG BASE_IMAGE` works only for the parts shared by all agents (apt packages, dotfiles).
- **Ambiguous pieces kept in `base`:** oh-my-posh (only the Claude status line uses it today)
  and Node (needed for npm installs, but a non-npm agent would not need it).
  `permissions.defaultMode: auto` went to customize. If cbox wants it as a sandbox-wide default,
  it belongs in `images/claude/settings.json`.
- **The `cc` link.** It is kept in `claude` as in `base/`. It still shadows the C compiler, which
  matters for a future Rust variant (see `base-rust/`'s `CC=gcc`). Dropping it is cleaner.
- **How `cbox up` / `just build` switch over.** Not done. The image names, the
  `-- <cmd>`/`exec` default command per agent (`claude` vs `codex` vs `opencode`), and a
  profile `image` field are all candidates. `base/` and `custom/` are deliberately left
  untouched here.
- **Releases and CI.** Not done. Possible shape: a `changes` filter for `^images/`, a
  `docker/bake-action` job on the existing native amd64/arm64 runners, and push-by-digest plus
  `imagetools create`, as `image.yml` does now, under names such as
  `ghcr.io/the-mentor/cbox-agent-<name>`. Tags, the weekly refresh and cache rules would follow
  `docs/design/images.md`.
- **Migration from `base/` + `custom/`:** (1) accept this design and add CI for `images/`;
  (2) publish `cbox-agent-claude`; (3) rebase `custom/` onto it (`CBOX_BASE_IMAGE`), moving its
  defaults out (onboarding, trust and the auto-updater are already in `claude`); (4) port
  `base-rust/` onto `base`; (5) remove `base/`, update `docs/design/images.md`,
  `docs/design/general.md` and `AGENTS.md`.
- **File duplication.** `images/base/bashrc.d/boxlite.sh` and `images/claude/claude-wrapper.sh`
  are copies of the files in `base/`, because the build context is `images/` and `base/` must
  not change in this PR. They can drift until `base/` is removed.

### Unverified

- **Nothing was built.** This machine has no `docker` and no `docker buildx`
  (`docker: command not found`), so neither `docker buildx bake --print` nor
  `docker buildx build --check` was run. Downloading a standalone `buildx` binary to run
  `--print` was blocked. The only checks were static: every `COPY` source exists, and the JSON
  files parse.
- Whether `@opencode/cli`'s npm install works as root with `npm install -g` (its pnpm/bun
  instructions mention allowing its build/postinstall step), and whether its platform binary
  resolves on both amd64 and arm64.
- Whether Bake resolves `context = "images"` relative to the current directory, as the README
  assumes (run from the repo root), or relative to the bake file.
- Whether Codex's npm package still runs `codex` in this image without further setup.
- The codex and opencode docs list a `curl` script and npm side by side, and neither says
  which one it prefers. npm was a choice, not a documented recommendation.
