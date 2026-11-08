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
| `env` | Plain, non-secret environment variables for the box, as `"KEY=VALUE"` strings (same meaning as `up -e`, except that a bare `KEY` host lookup is not allowed in a profile). |

Every field is optional. Volumes (`-v`) are **not** in a profile in this version (see
Follow-ups).

## Goals

- Save and reuse a set of secrets, an image, resource sizes and plain environment
  variables (a git identity, say) under a name.
- A profile can only *use* a secret. The secret's allowed hosts and env var name stay
  with the secret and a profile cannot change them (`secrets.md`: "a profile or flag
  that can widen a secret's allowed hosts" is an anti-goal).
- No secret value is ever written to `profiles.toml`. It holds secret names.
- A box that was created from a profile tells the user, on resume, how the profile
  differs from the box.

## Non-goals

- Inheritance between profiles. There is none.
- Volumes in profiles (follow-up issue).
- Keeping credentials out of `env` by force. cbox warns about what looks like one; it never
  refuses.
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
- `-e KEY=VALUE` **overrides** a profile `env` entry with the same name for that run.
  Other `-e` flags add to the profile's variables.
- `-v`, `-c`, `-f`, `-d`, `-- <cmd>` are unchanged (not part of profiles).

The `NAME=ENV@hosts` full-form flag still works as a one-off `--secret` and is not
valid inside a profile.

### Secrets inside a profile

- Only stored secrets can be named. `gh` and `gh_basic` are reserved and rejected.
- `profile create` and `profile edit` check each named secret exists, using the names-only
  index (`secrets.json`), not the OS store. A missing name fails the command and
  nothing is written.
- A secret deleted later is not tracked. For a new box, `cbox up --profile foo` fails
  validation with `profile foo: secrets entry N is not a stored secret (see cbox secret list) (<profiles.toml path>)` (entries are reported by position, never by text, so a value pasted into the list by mistake is not echoed).
  Resuming a box whose profile lists it only warns (see Resuming a box). The store's
  "secret not found" error only appears if the index and the OS store disagree.
- `cbox secret delete NAME` prints a separate line to stderr before the prompt, with or
  without `-y`: `cbox: warning: secret "NAME" is used by profile(s): a, b. ...`. The
  prompt itself is just `Delete secret "NAME"? [y/N]`. It does not refuse.

### Environment variables inside a profile

```toml
[work]
secrets = ["openai"]
env = [
  "GIT_AUTHOR_NAME=Avri Chen-Roth",
  "GIT_AUTHOR_EMAIL=11185446+the-mentor@users.noreply.github.com",
]
```

- `env` is an array of `"KEY=VALUE"` strings, split at the first `=` (a value may contain
  more `=`). Order is kept. The variables reach the box the way `up -e` ones do.
- Checks (`validate`, `create`, `edit`, `up`): an entry without `=`, a name that is not
  letters, digits and `_` (not starting with a digit), or the same name twice is an error.
  The name rule is stricter than `up -e`, which only rejects an empty name. A well-formed
  variable is named in the error, since a name is not secret (`env GIT_AUTHOR_NAME is set
  twice`). A malformed entry (no `=`, or an invalid name) is reported by position
  (`env entry 2 ...`) and never by its text, because a bare token pasted without a
  variable name would otherwise be split at its first `=` and its first part echoed as the
  "name". An entry whose value is empty
  or only `=` characters and whose name is 16 or more characters with no `_` (`xxxx...==`,
  padded base64) is treated as a token pasted without `NAME=` and rejected, also by
  position (`env entry 2 looks like a token pasted without a NAME= prefix`). One rule
  (`env_pair`) serves every consumer. A residual risk is accepted: a token of plain
  letters and digits with a value that is not just `=` characters passes the rule, and
  would show in `profile list`, the duplicate error, the credential warning and the box
  environment.
- **Credential warning (warn, never refuse).** A variable is flagged when its name has
  `KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `PASS`, `PASSPHRASE`, `CREDENTIAL`,
  `CREDENTIALS`, `AUTH`, `BEARER`, `PRIVATE`, `PAT`, `APIKEY`, `APITOKEN`, `ACCESSKEY` or
  `SECRETKEY` as a whole word (split on `_`, any case), or when its value starts with
  `sk-`, `ghp_`, `github_pat_`, `AKIA`, `glpat-`, `npm_`, `AIza`, `eyJ`, `xoxb-`, `xoxp-`,
  `xoxa-`, `xoxs-` or `xoxr-`. The warning says tokens and secrets should not go in environment variables and
  to use `cbox secret`; it names the variable, never the value. It is printed by
  `profile create`, `edit` and `validate`, not by every `up`. The check is a heuristic: it
  will miss some and flag some wrongly, and there is no entropy test (a git email scores
  higher than a real token).
- `profile list` prints `KEY=VALUE`; a flagged variable prints `KEY=<hidden>`.
- The box's sidecar records, for each variable, its name and a hash of its value, never
  the value, so a flagged token cannot land in it. The resume warning lists variables
  added, removed or changed, by name only.

### Commands

```
cbox profile create NAME [--secrets a,b] [--image IMG] [--memory GB] [--cpus N] [--disk-size GB]
                         [--env KEY=VALUE ...]
cbox profile list
cbox profile edit NAME [--secrets a,b | --add-secrets a,b | --remove-secrets a,b]
                       [--image IMG] [--memory GB] [--cpus N] [--disk-size GB]
                       [--env KEY=VALUE ...] [--unset-env NAME,NAME]
                       [--reset secrets,image,cpus,memory,disk-size,env]
cbox profile delete NAME [-y]
cbox profile validate [NAME]
```

- `--secrets` takes a comma-separated list; one name is `--secrets a`.
- On `edit`, `--secrets` replaces the list, `--add-secrets` / `--remove-secrets` change it
  in place (mirrors `secret edit --host/--add-host/--remove-host`). `--secrets` cannot be
  combined with the other two.
- `--env KEY=VALUE` is repeatable (values may contain commas). On `edit`, a name that is
  already set has its value replaced and the others are kept; `--unset-env A,B` removes
  variables (an unknown name is an error); `--env X=1 --unset-env X` conflicts.
- `--reset FIELDS` (comma-separated: `secrets`, `image`, `cpus`, `memory`, `disk-size`, `env`) stops
  setting those fields, so cbox's built-ins apply (`secrets` empties the list). Resetting a
  field that is not set is fine. Combining `--reset X` with the flag that sets X is an error
  (`--reset secrets` conflicts with all three secrets flags, `--reset env` with `--env` and
  `--unset-env`); `--reset image --cpus 4` is fine.
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

- A profile that no longer validates (a listed secret was deleted, a hand-edited bad
  value) does not stop a resume. "Resume" means the box has a sidecar and `-f` was not
  passed, with or without `--profile`. `up` prints each problem as a `cbox: warning:`
  line, says the box keeps the settings it was created with and that this run ignores
  the missing secrets and the invalid env entries, and points to `-f`. Profile secrets the index does not know (or
  that are not a valid name) are left out of this run, so they show as removed in the
  diff below; a bad profile resource value falls back to the built-in for this run (the
  resumed box ignores it anyway). A `--secret X` flag for a missing `X` still fails, and
  so does a bad `--cpus`/`--memory`/`--disk-size` flag.
- A new box, and `-f`, keep the hard failure.
- If the sidecar outlived its box (the box was removed some other way), the run that
  downgraded the problems ends up creating a box. `up` then removes that box and fails
  with the same error a new box gets.

- At creation, the box's sidecar records the resolved profile: profile name, secret
  names, image, CPUs, memory, disk size, and for `env` each variable's name and a hash of
  its value.
- With no `--profile` on a resume, `up` uses the profile name recorded in the box's
  sidecar (not `default`), so resuming a box made from `client-a` compares against
  `client-a`'s current contents and is quiet if nothing changed. `up -f NAME` without
  `--profile` also recreates the box from its recorded profile, and the "recorded profile
  no longer exists" refusal applies to `-f` too. A new box with no
  `--profile` uses `default`. With `--profile X` on a resume, `up` compares against `X`,
  and says so when `X` is not the profile the box was created from.
- On resume, `up` resolves that profile again, with the flags of this invocation, and
  prints the differences: secrets added or removed, variables added, removed or changed
  (names only), and changed image, CPUs, memory or disk size.
  The message ends with "run with -f/--force to recreate it".
- This closes a gap: today added or dropped secrets and any resource change are not
  reported at all.
- The existing per-secret hash warning stays.

## Anti-goals

- Any path where a profile changes a secret's hosts or env name.
- A secret value, or the env-name-to-value pair, written to `profiles.toml`.
- A credential value printed by cbox. `env` can hold one if the user puts it there, so
  the warning, the hidden value in `list`, and the hash in the sidecar exist.

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
  error naming the file, line and column, e.g. `invalid <path>: line 4, column 1: not
  valid TOML or not a valid profile (unknown key or wrong type); fix or remove that
  entry`. It never quotes the file: no source line, key or value (a value pasted by
  mistake would otherwise reach the terminal). `up` fails on it; there is no fallback to built-ins.
  `secret delete` warns that it could not check the profiles and still deletes.
- Boxes created before this feature have no recorded profile in their sidecar. The new
  sidecar fields are optional, and on resume the profile diff is skipped for them. A box
  that recorded a profile but predates `env` tracking (its snapshot has no `env`) gets one
  line when the profile now has variables: `env: this box predates env tracking and may
  lack: A, B` (names, sorted). It is silent when the profile has no variables.
- Profile values are validated with the same limits as the flags (memory at least 1,
  `cpus` 1 to 255, disk size at least 1), by the same code, at `create`/`edit` and at `up`.
- A missing `secrets.json` counts as no secrets, so `profile create --secrets a`
  fails with the usual "is not a stored secret" error. A zero-byte or malformed one is an
  error (`invalid index <path>`), for `profile create`/`edit`/`validate` and for `up`
  with a profile that names secrets.
- A full-form `--secret NAME=ENV@hosts` whose NAME is already in the profile fails the
  same way as a plain duplicate. Env-name collision checks run over the merged set.
- `profile list` is plain text, one block per profile.
- `--image` on `up` today has a default value, so it cannot tell "not passed" from
  "passed the default". Making it optional is part of this work.
- A profile's secret list is kept in the order given, duplicates rejected.
- `profile list` shows full contents rather than only names; there is no `show`.
- Unknown keys in `profiles.toml` are an error, not ignored, so a typo like `memroy`
  does not silently do nothing.

- `env` is an array of `"KEY=VALUE"` strings and not a TOML table (the user's choice): it
  matches `secrets = [...]` and the `up -e` syntax.
- `--unset-env` takes a comma-separated list of names (names have no commas); `--env` is
  repeated instead, because a value may contain commas.
- Variable names must match `[A-Za-z_][A-Za-z0-9_]*` in a profile. `up -e` accepts any
  non-empty name; `up` applies the profile rule to profile entries only.
- The credential check is names plus a short prefix list. No entropy test, no new
  dependency (user: it must be cheap to implement).

## Follow-ups (not in this work)

- Volumes (`-v`) in profiles. One issue, to be opened by the user.
- A profile that selects a box name or a working-directory mount.
- `CBOX_PROFILE` environment variable to pick a profile.
