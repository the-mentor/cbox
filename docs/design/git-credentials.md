# Git and GitHub credentials from stored secrets

## Intent

Today GitHub access in a box is hard-coded: if the host shell has `GH_TOKEN` or
`GITHUB_TOKEN`, `cbox up` builds two reserved secrets (`gh` for `api.github.com`,
`gh_basic` for `github.com`) and a fixed git bootstrap (`secrets.rs`: `github_spec`,
`build`, `git_bootstrap_script`; call sites in `commands/up.rs`). One host token, one
account, no per-profile choice, no self-hosted GitHub, no other git provider.

The owner wants different GitHub (and git) credentials per profile, using `cbox secret`
for the token and the existing profile mechanism to pick it. A box started from profile
`work` should use `github_work`; one from `personal` should use `github_personal`. `gh` and
`git` must both work, the real token must stay host-side (the box only sees placeholders),
and nothing may be GitHub-specific, so GitLab, Gitea and self-hosted instances work too.

## Goals

- A stored secret can declare that git should use it. cbox derives what git needs and
  configures git in the box.
- Profiles choose which secret a box gets, with no change to profile structure.
- Nothing in code or config names GitHub. Hosts, env var and username come from the secret.
- The reserved `gh`/`gh_basic` special case and the host `GH_TOKEN`/`GITHUB_TOKEN`
  auto-detection are removed.

## Non-goals (deferred, not rejected)

- A `cbox git-config` subcommand or any new stored kind. `--git` on a secret is enough.
- Git identity (`user.name`, `user.email`, `GIT_AUTHOR_*`). Stays in profile `env`.
- `--base64-env` (exposing the encoded value as an env var, e.g. for Jira Cloud). Likely
  next; it is one more optional field on the same secret and needs no redesign.
- Baking anything into the images, and `gh auth setup-git` in the base image (PR #161
  should be closed or reduced to its `custom/Dockerfile` change; see Open items).
- Delivering git config through `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n` env vars. Keep running
  `git config` in the guest as `up.rs` does now.

## Anti-goals

- The real token (or its base64 form) in the box, in an image layer, or in the index file.
- A box that has no git secret getting a `Basic <placeholder>` header with nothing to
  substitute it: that makes anonymous clones of public repos return 401
  (`docs/design/cbox.md`, "Why cbox applies these at runtime").
- A baked-in `gh auth git-credential` helper: with placeholders it sends base64'd
  placeholder garbage (`docs/design/cbox.md`, "Required follow-up on the image").

## Behaviour

### Creating a secret

```
cbox secret create github_work \
  --host github.com,api.github.com,gist.github.com \
  --env GH_TOKEN --user x-access-token --git
```

- `--host` is as today (multiple allowed, bare host names). Hosts are the proxy
  substitution hosts and, with `--git`, also the git hosts.
- `--env` becomes **optional**. A secret must have at least one of `--env` or `--git`;
  otherwise create/edit rejects it as it would do nothing.
- `--git` (new, flag): cbox derives `base64("<user>:<token>")` and wires git for every
  host in `--host`.
- `--user` (new): the Basic username. **Required with `--git`, no default**, to stay
  provider-neutral (GitHub `x-access-token`, GitLab commonly `oauth2`; an empty username
  must be allowed, e.g. Azure DevOps uses `base64(":PAT")`). Rejected without `--git`.
- `edit` gets the same flags (`--user`, `--git`, and a way to turn `--git` off); changing
  them leaves the stored token alone, as `--host`/`--env` do today.
- Names ending `_basic` become reserved (derived secret names).

### Profiles

Unchanged. A profile lists secret names; `work` lists `github_work`, `personal` lists
`github_personal`. `cbox up --secret github_work` works the same way.

### At `cbox up`, for each stored secret with `--git`

Host side, two SDK secrets on the same hosts:

| Name | Value | Placeholder |
|---|---|---|
| `<name>` | raw token | `<BOXLITE_SECRET:<name>>` |
| `<name>_basic` | `base64("<user>:<token>")` | `<BOXLITE_SECRET:<name>_basic>` |

Guest side, run once at box creation (non-fatal, probe for git first, warn and skip if
absent, exactly as the current bootstrap), for every host in the secret's host list:

```
git config --global http.https://<host>/.extraHeader 'Authorization: Basic <BOXLITE_SECRET:<name>_basic>'
git config --global credential.https://<host>.helper ''
```

The blank helper is scoped to the host URL, not generic `credential.helper`
(`secrets.rs`, doc comment on `git_bootstrap_script`, for the reason). If `--env` is set,
the env var carries `<BOXLITE_SECRET:<name>>`; with `--git` only, no env var is set.

Writing the line for every host (including ones git never uses, such as
`api.github.com`) is deliberate: it is harmless because the config is URL-scoped, and it
avoids a second host list. Everything is https-only; substitution only works through the
TLS proxy.

### Conflicts

`check_unique_env` already rejects two secrets on one env var; extend the overlap check so
two `--git` secrets in one box whose hosts overlap are rejected at `up` with a clear error.
Secrets with no env var are skipped by the env-var check.

### Resume / change detection

`resume_hashes` must include the new fields (`user`, `git`) for stored secrets so editing
them shows the existing "changed since this box was created" warning.

## Constraints and code touchpoints

- Storage: the index file holds names only; hosts and env live in the secret store item
  (`StoredSecret { value, hosts, env }`, `secretstore/mod.rs`, with the keyring, WSL
  `wincred` and `FakeStore` backends). The new fields (`user`, `git`) belong with
  `hosts`/`env`, not in the index. Existing stored secrets must keep loading: new fields
  default to "none/false".
- `SecretSpec.env_var` (`secrets.rs`) becomes `Option<String>`; `build`, `source_vars`,
  `passthrough_without_stored` and `check_unique_env` handle `None`.
- Remove: `github_spec`, `has_github_token`, the `gh`/`gh_basic` branch in `build`,
  `GH_BASIC_USER`, `RESERVED_NAMES` entries `gh`/`gh_basic` (replace with the `_basic`
  suffix rule), the `has_github_token` call in `commands/up.rs`, and their tests.
- `git_bootstrap_script` becomes a function of the git-enabled secrets (host list and
  placeholder per secret); `commands/up.rs` runs it when any exist.
- `commands/up.rs` (decided by the owner): remove the `has_github_token()`/`github_spec()`
  push (line ~81). `has_github` (line ~88, `name == "gh"`) becomes "any spec has `--git`",
  and that drives the guest bootstrap (line ~236 and the script run near line ~394). Its
  warnings say "GitHub"; make them provider-neutral. A `GH_TOKEN`/`GITHUB_TOKEN` in the
  env file (`~/.config/cbox/env`, `--env-file`, `$CBOX_ENV_FILE`) no longer gives the box
  any GitHub access; the token must be a stored secret. The env file keeps working for
  `--secret NAME=ENV@hosts`, passthrough vars and `-e`.
- Platforms without a secret store (Linux desktop is not implemented; `cbox secret` fails
  with "no secret store available") lose GitHub access in the box. Accepted by the owner
  (macOS and WSL only for now); note it in the release notes.
- `just build-image` keeps using `GH_TOKEN`/`gh auth token` as a build-time BuildKit secret
  for private plugins; unrelated and unchanged.
- The flag form `--secret NAME=ENV@hosts` (value from a host env var) is unchanged and does
  not get `--git`.
- Hosts are interpolated into a shell command: validate as plain host names with an
  optional `:port` (no quotes, spaces, `/`, shell metacharacters).
- Docs to update: `docs/design/cbox.md` (the "GitHub moves to secrets" section),
  `docs/design/secrets.md`, `docs/design/general.md` (build-time token description),
  `README.md`/`AGENTS.md` mentions of `gh`/`gh_basic` and `GH_TOKEN` auto-detection.
- Migration is a breaking change: a host with `GH_TOKEN` exported but no GitHub secret
  assigned gets no GitHub access in the box after this change. State it in the release
  notes (Conventional Commit `feat!:`/`BREAKING CHANGE`).

## Open items

- **Custom ports unverified.** Git's URL-scoped config matches `host:port`. Whether
  boxlite strips the port when matching a secret's hosts, or compares the full string, is
  unknown (`parse_hosts` accepts `host:port`; its comment says boxlite matches bare host
  names). Test on the first self-hosted instance on a non-standard port; the fix may be to
  allow the git host (with port) to differ from the proxy host.
- **Self-hosted `gh` env var.** `gh` reads `GH_TOKEN` for `github.com` and `*.ghe.com`, and
  `GH_ENTERPRISE_TOKEN` for GitHub Enterprise Server (verified via `gh help environment`).
  The user chooses `--env`, so no code is needed, but document it. `GH_HOST` may also be
  needed as a profile `env`; not verified. Server API path and `ghe.com` API host names
  not verified.
- **Optional `--git` host subset** (`--git github.com,gist.github.com`) if a mixed-host
  secret turns up. Start with bare `--git` = all hosts.
- **PR #161** (`gh auth setup-git` in the base image, `GH_TOKEN` in `custom/Dockerfile`):
  decision pending from the owner. Under this design, close it, or keep only the
  `custom/Dockerfile` build-time change.

## Left to the builder

Flag parsing and validation messages, the derived-name collision wording, the on-disk
shape of the new `StoredSecret` fields and how to read old items, test layout, and the
exact `up` output when a secret's git wiring is skipped.
