# Profiles: design

Status: implemented. Follows `docs/design/secrets.md`, which deferred profiles to
"their own design pass".

## Intent

`cbox up` takes a growing pile of flags: an image, several `--secret NAME`, memory, CPUs,
disk size. People who use the same set for a kind of work (a client project, a personal
sandbox) retype it every time. A **profile** is a named, saved bundle of those choices,
selected with `cbox up --profile foo`.

Profiles also let a user keep sets apart on purpose: a client profile that never carries
the user's personal GitHub token is a feature, not an accident.

## What a profile holds

| Field | Meaning |
|---|---|
| `secrets` | Names of stored secrets (see `secrets.md`). Names only, never values or hosts. |
| `image` | Image the box boots (same meaning as `up -i`). |
| `cpus`, `memory` (GiB), `disk_size` (GB) | Same meaning as `up --cpus/--memory/--disk-size`. |

Every field is optional. Environment variables (`-e`) and volumes (`-v`) are **not** in
a profile in this version (see Follow-ups).

## Goals

- Save and reuse a set of secrets, an image, and resource sizes under a name.
- A profile can only *use* a secret. The secret's allowed hosts and env var name stay
  with the secret and a profile cannot change them (`secrets.md`: "a profile or flag
  that can widen a secret's allowed hosts" is an anti-goal).
- No secret value is ever written to `profiles.toml`. It holds secret names.
- A box that was created from a profile tells the user, on resume, how the profile
  differs from the box.

## Non-goals

- Inheritance between profiles. There is none.
- Environment variables and volumes in profiles (follow-up issue).
- Per-project profile files checked into a repo. One per-user file only.
- Profile-level host overrides, ever.

## Rules

### Where profiles live

One file, `profiles.toml`, next to `secrets.json`: `$XDG_CONFIG_HOME/cbox/profiles.toml`,
else `~/.config/cbox/profiles.toml` (the same lookup as `config::secrets_index_path`).

```toml
[default]

[client-a]
secrets = ["anthropic", "client-a-gh"]
image = "ghcr.io/example/cbox-rust:latest"
memory = 8
cpus = 4
```

### The default profile

- `default` always exists. cbox creates it, empty, when it first runs and the file is missing (see Calls made by the author).
- `cbox up` with no `--profile` uses `default`.
- `default` can be edited like any profile. `cbox profile delete default` is refused.

### No inheritance

A profile is cbox's built-in values plus what that profile sets, and nothing else.
`--profile foo` never sees anything from `default`. The built-ins are the ones `up` uses
today: the `cbox-custom` image, 4 GiB, 2 CPUs, 10 GB disk.

So a client profile that omits `anthropic` does not get it, and `default`'s memory
override does not apply to `foo`.

### Flags on `up`

Flags apply on top of the chosen profile for that run only and never change
`profiles.toml`.

- `--secret NAME` **adds** to the profile's secrets. If the profile already lists
  `NAME`, `up` fails: `secret NAME is already in profile foo`.
- `-i`, `--memory`, `--cpus`, `--disk-size` **override** the profile's value.
- `-e`, `-v`, `-c`, `-f`, `-d`, `-- <cmd>` are unchanged (not part of profiles).

The `NAME=ENV@hosts` full-form flag still works as a one-off `--secret` and is not
valid inside a profile.

### Secrets inside a profile

- Only stored secrets can be named. `gh` and `gh_basic` are reserved and rejected.
- `profile create` and `profile edit` check each named secret exists, using the names-only
  index (`secrets.json`), not the OS store. A missing name fails the command and
  nothing is written.
- A secret deleted later is not tracked. `cbox up --profile foo` fails with the existing
  "secret not found" error.
- `cbox secret delete NAME` names the profiles that use `NAME` in its confirmation
  prompt. With `-y` it prints the same warning and proceeds. It does not refuse.

### Commands

```
cbox profile create NAME [--secrets a,b] [--image IMG] [--memory GB] [--cpus N] [--disk-size GB]
cbox profile list
cbox profile edit NAME [--secrets a,b | --add-secrets a,b | --remove-secrets a,b]
                       [--image IMG] [--memory GB] [--cpus N] [--disk-size GB]
cbox profile delete NAME [-y]
cbox profile validate [NAME]
```

- `--secrets` takes a comma-separated list; one name is `--secrets a`.
- On `edit`, `--secrets` replaces the list, `--add-secrets` / `--remove-secrets` change it
  in place (mirrors `secret edit --host/--add-host/--remove-host`). `--secrets` cannot be
  combined with the other two.
- `create` refuses a name that exists. `edit` and `delete` fail on a missing name.
- `delete` confirms first, like `secret delete`. `delete default` is refused.
- `list` prints every profile with its contents.
- `validate` checks `profiles.toml` without starting anything: it parses the file
  (syntax, unknown keys, wrong types), applies the flag limits to every value, rejects
  `gh`/`gh_basic`, and checks that each named secret exists in the index. With NAME it
  checks that profile only; without, all of them. It reports every problem it finds
  (not only the first), names the profile and field, and exits 0 when clean, non-zero
  otherwise. It reads no secret values and never writes. A missing file is valid (empty
  `default`). A TOML syntax, key or type error is reported alone, since the file cannot be
  read past it.
- `create`, `edit` and `delete` rewrite the whole file: comments are dropped, and a
  symlinked `profiles.toml` becomes a regular file.
- Nothing prints a secret value (none are read).

### The `just` recipe

`just profile ...` forwards its arguments to `cbox profile`, exactly like `just secret`
does for `cbox secret` (same guard for a missing binary, same positional-argument
handling so `--secrets a,b` stays one argument). It is added to the `justfile`, to the
`just` command list in `AGENTS.md`, and to the usage comment above the recipe.

### Resuming a box

A box keeps the settings it was created with. This is already true for secrets, and
`up` already warns when a stored secret's value, hosts or env name changed since the box
was created (`sidecar::changed_secrets`, `up.rs`). Profiles follow the same rule: resume
works and never refuses, and `up` prints a warning showing what differs. One accepted
exception: with no `--profile`, `up` refuses to resume a box whose recorded profile no
longer exists in `profiles.toml`; the error says to pass `--profile NAME`.

- At creation, the box's sidecar records the resolved profile: profile name, secret
  names, image, CPUs, memory, disk size.
- With no `--profile` on a resume, `up` uses the profile name recorded in the box's
  sidecar (not `default`), so resuming a box made from `client-a` compares against
  `client-a`'s current contents and is quiet if nothing changed. A new box with no
  `--profile` uses `default`. With `--profile X` on a resume, `up` compares against `X`,
  and says so when `X` is not the profile the box was created from.
- On resume, `up` resolves that profile again, with the flags of this invocation, and
  prints the differences: secrets added or removed, and changed image, CPUs, memory or disk size.
  The message ends with "run with -f/--force to recreate it".
- This closes a gap: today added or dropped secrets and any resource change are not
  reported at all.
- The existing per-secret hash warning stays.

## Anti-goals

- Any path where a profile changes a secret's hosts or env name.
- A secret value, or the env-name-to-value pair, written to `profiles.toml`.
- `profiles.toml` holding anything that could be mistaken for a credential. That is why
  environment variables are out of this version.

## Constraints

- No TOML dependency exists today (`secrets.json` and `registries.json` are JSON).
  `profiles.toml` needs one. **Author's call:** the `toml` crate with serde, already
  the idiomatic choice next to `serde`. A fixed schema with unknown keys rejected.
- `profiles.toml` is not a secret store, but it is still written atomically (temp file
  in the same directory, then rename) so a crash cannot leave a half-written file.
- Single user, single machine. No locking beyond the atomic rename.

## Calls made by the author (not decided by the user)

- Profile names use the secret name charset: `[a-z][a-z0-9_-]*`.
- The file location follows `secrets.json` (XDG then `~/.config`).
- If `profiles.toml` is missing, cbox creates it with an empty `[default]` the first time
  it runs, with or without a subcommand (`cbox`, `cbox --help`, `cbox secret list`,
  `cbox up`, all of them). It happens at startup, before the command runs. If the file
  exists but has no `[default]`, it is treated as an empty `default` and `[default]` is
  added on the next profile write. Startup creation is best-effort: if the file cannot be
  written (read-only home, no permission), cbox prints one warning and carries on with an
  empty `default`; no command fails because of it. Startup never overwrites or edits an
  existing file.
- A `profiles.toml` that does not parse (syntax error, unknown key, wrong type) is an
  error naming the file and line. `up` fails on it; there is no fallback to built-ins.
  `secret delete` warns that it could not check the profiles and still deletes.
- Boxes created before this feature have no recorded profile in their sidecar. The new
  sidecar fields are optional, and on resume the profile diff is skipped for them.
- Profile values are validated with the same limits as the flags (memory at least 1,
  `cpus` 1 to 255, disk size at least 1), by the same code, at `create`/`edit` and at `up`.
- A missing or empty `secrets.json` counts as no secrets, so `profile create --secrets a`
  fails with the usual "no secret named a" error.
- A full-form `--secret NAME=ENV@hosts` whose NAME is already in the profile fails the
  same way as a plain duplicate. Env-name collision checks run over the merged set.
- `profile list` is plain text, one block per profile.
- `--image` on `up` today has a default value, so it cannot tell "not passed" from
  "passed the default". Making it optional is part of this work.
- A profile's secret list is kept in the order given, duplicates rejected.
- `profile list` shows full contents rather than only names; there is no `show`.
- Unknown keys in `profiles.toml` are an error, not ignored, so a typo like `memroy`
  does not silently do nothing.

## Follow-ups (not in this work)

- Environment variables (`-e`) and volumes (`-v`) in profiles. One issue, to be opened
  by the user.
- A profile that selects a box name or a working-directory mount.
- `CBOX_PROFILE` environment variable to pick a profile.
