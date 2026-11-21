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
| `base/`: apt packages, `gh`, `uv`, `pre-commit`, the `sandbox-user` rename, `/workspace` | `base` (`yq` is new in `images/`, from apt: the Python jq-wrapper, v3.4.3 on trixie) | Not agent-specific. |
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
| Claude Code | `npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}` | `2.1.330` | https://code.claude.com/docs/en/setup#install-with-npm |
| Codex | `npm install -g @openai/codex@${CODEX_VERSION}` | `0.171.0` | https://github.com/openai/codex (README: npm, a `curl` install script, or Homebrew) |
| opencode | `npm install -g @opencode/cli@${OPENCODE_VERSION}` | `2.3.2` | https://opencode.ai/docs/ (npm, a `curl` install script, Homebrew, and others) |

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
- **File duplication.** `images/base/bashrc.d/boxlite.sh`, `images/claude/claude-wrapper.sh` and
  `images/claude/cbox-hook.sh` are copies of files in `base/` (and `custom/` for the hook),
  because the build context is `images/`. `base/` and `custom/` stay in use until they are
  removed, so the copies can drift until then.

### Unverified

- **Nothing was built.** This machine has no `docker` and no `docker buildx`
  (`docker: command not found`), so neither `docker buildx bake --print` nor
  `docker buildx build --check` was run. Downloading a standalone `buildx` binary to run
  `--print` was blocked. The only checks were static: every `COPY` source exists, and the JSON
  files parse.
- (The package name is confirmed: opencode.ai/docs lists `npm install -g @opencode/cli`.) Whether `@opencode/cli`'s npm install works as root with `npm install -g` (its pnpm/bun
  instructions mention allowing its build/postinstall step), and whether its platform binary
  resolves on both amd64 and arm64.
- Whether Bake resolves `context = "images"` relative to the current directory, as the README
  assumes (run from the repo root), or relative to the bake file.
- Whether Codex's npm package still runs `codex` in this image without further setup.
- The codex and opencode docs list a `curl` script and npm side by side, and neither says
  which one it prefers. npm was a choice, not a documented recommendation.
