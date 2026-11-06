# cbox secrets: design

Status: draft for approval. Nothing here is built yet.

## Intent

Today a secret reaches a box only if the user exports its value as a host
environment variable and passes `cbox up --secret NAME=ENV_VAR@hosts` on every
run. That works, but the value has to sit in the shell environment (or an env
file) and the flag has to be retyped each time.

This design makes secrets something cbox manages: `cbox secret create | list |
edit | delete`, with values held in the operating system's credential store, and
`cbox up --secret NAME` resolving them from there.

This is the first step toward **profiles** (a named bundle of source image,
secrets, and resource overrides, selected with `cbox up --profile foo`). Profiles
are explicitly out of scope here, but the secret record and the `up` lookup are
shaped so a profile can later refer to a secret by name without a migration.

## Goals

- `cbox secret create`, `list`, `edit`, `delete`.
- Secret values are never written to disk in plaintext by cbox.
- macOS: the Keychain. WSL: Windows Credential Manager.
- `cbox up --secret NAME` takes the value from the store and feeds the existing
  substitution path (`cbox/src/secrets.rs`), unchanged. The box still only sees a
  placeholder; the host-side proxy swaps in the real value, only for the secret's
  allowed hosts.
- The existing `--secret NAME=ENV_VAR@host[,host...]` form keeps working exactly
  as today.
- Each stored secret carries its allowed hosts. A secret with no hosts cannot be
  created (cbox already refuses unscoped secrets).

## Non-goals (this design)

- Profiles, and any `--profile` flag.
- Linux desktop (Secret Service) support. Designed for, built later.
- `--from-env` on `create`/`edit`. Deferred by decision; see Follow-ups.
- Certificates, SSH keys, or any secret over a few KB. Secrets are short strings
  (API keys, tokens).
- Sharing or syncing secrets between machines.

## Anti-goals (a failure even if it technically works)

- Any path that writes a secret value to disk unencrypted, including a fallback.
- A secret value on a command line (visible in `ps`, shell history).
- A command that prints a stored value.
- A profile or flag that can widen a secret's allowed hosts.

## What the user sees

```
$ cbox secret create openai --host api.openai.com --env OPENAI_API_KEY
Value (input hidden): ********
Saved "openai" in macOS Keychain, allowed hosts: api.openai.com

$ cbox secret list
NAME     HOSTS              ENV               STORE
openai   api.openai.com     OPENAI_API_KEY    macOS Keychain

$ cbox secret edit openai                          # prompt for a new value
$ cbox secret edit openai --host a.com,b.com       # replace the whole host list
$ cbox secret edit openai --add-host c.com
$ cbox secret edit openai --remove-host b.com
$ cbox secret delete openai                        # confirms; -y skips

$ cbox up --secret openai
```

Details decided by the user:

- `--host` is a single **comma-separated** list (no repeated flag). Trim spaces,
  drop empty entries and duplicates, as `parse_secret_flag` does today.
- On `edit`, `--host` **replaces** the whole list; `--add-host` adds;
  `--remove-host` removes. `edit` prints the old and new list whenever hosts
  change. `edit` refuses to leave a secret with no hosts.
- The value is entered at a hidden prompt, or read from stdin when piped
  (`pbpaste | cbox secret create openai --host api.openai.com`).
- `cbox secret create --help` shows a few examples (prompt, several hosts, piped).
- Names are one namespace shared with the existing flag form; `gh` stays reserved.

## Constraints

- cbox is Rust (edition 2024, clap, tokio). Platforms: macOS, Linux, Windows via
  WSL. cbox itself cannot be built for native Windows (BoxLite).
- A WSL distro usually has no Secret Service daemon, so the stock `keyring`
  crate fails there. Verified by spike: `NoDefaultStore`.
- Commit and PR titles use Conventional Commits (release-please).

## Storage design

**Backends.** Use `keyring-core` (the API crate, v1.0.x) plus a store crate per
platform, not the all-in-one `keyring` crate. Register one store at startup with
`set_default_store`.

| Platform | Store |
|----------|-------|
| macOS | `apple-native-keyring-store` (Keychain) |
| WSL | a small store written in cbox over Windows Credential Manager |
| Linux desktop | `zbus-secret-service-keyring-store` (later) |
| anything else | `cbox secret ...` fails with a clear "no secret store available" error |

**WSL store.** Validated by spike (`spike/keyring-wsl`, `spikes/wincred-store/`):
a custom `keyring-core` store that runs `powershell.exe` through WSL interop and
calls `CredWriteW/CredReadW/CredDeleteW` via a compiled C# type. The secret goes
over stdin only, never argv. All six spike checks passed on a real WSL machine;
each call took about 0.5 s (almost all PowerShell startup). Credential Manager
caps a value at 2560 bytes (fine for tokens). The store must detect, before use:
WSL interop enabled, `powershell.exe` resolvable, `wslpath` present, and fail with
an actionable message otherwise. Reading several secrets should use one
PowerShell run, not one per secret.

**What is stored where.** One entry per secret in the OS store, keyed
`cbox/<name>`, whose stored value is a small JSON document holding the **value,
allowed hosts, and in-box env var name**. Keeping the hosts inside the protected
entry means anything able to widen a secret's hosts could equally read its value.
A separate plain file `~/.config/cbox/secrets.json` holds only the **list of
names** (so `list` can enumerate; `keyring-core` search is not implemented by
every store). If the index and the store disagree (entry removed in Keychain
Access, say), `list` shows the name as "missing value" and `delete` removes the
stale name.

**Use in `up`.** `--secret NAME` with no `=` and no `@` is a store lookup. The
form `NAME=ENV_VAR@hosts` is unchanged. A lookup builds the same `SecretSpec`
inputs `secrets::build` already consumes, with the value supplied by the store
instead of a host env var. A store-backed secret has no host source variable, so
the in-box env name is still added to the exclusion list `env::compose` takes as
`secret_vars`, so a host variable or `-e` flag of the same name makes `up` fail
instead of leaking the real value into the box (see "Details settled in review").
The resume warning needs a small change for store-backed secrets (same section).

## PR plan

Merge order matters only as noted: 1 before 2; 3 any time after 1.

1. **Store interface, macOS backend, `secret create | list | delete`.** On
   other platforms the commands print "not supported yet".
2. **`up` integration.** `--secret NAME` resolves from the store.
3. **WSL backend.** Detection, actionable errors, batched reads.
4. **`secret edit`** and `docs/design/cbox.md` updates.

## Follow-ups (not in this work)

- `--from-env VAR` on `create` and `edit`, defaulting `--env` to the same
  variable. Deferred by the user to a later PR.
- Linux Secret Service backend.
- Profiles (own design pass).
- A native Windows helper binary (startup in tens of milliseconds, no PowerShell)
  if PowerShell is blocked by policy on managed Windows machines (Constrained
  Language Mode breaks the C# compile) or latency matters.
- An encrypted-file fallback for headless Linux, if wanted.

## Calls made by the author (not decided by the user)

- Hosts are stored **with the secret**. Confirmed by the user: a profile only
  uses them and can never change them (no override, no narrowing, no widening).
- The in-box env var name is stored with the secret (`--env`); confirmed by the user.
- Hosts, env name, and value live together in the OS-store entry; a plain file
  holds only names.
- Linux with no store is an error, not a file fallback (confirmed by the user).
- No command prints a stored value (confirmed by the user: values never come back
  out of cbox except into a box through `up`).
- `--env` is **required** on `create` (confirmed by the user): no default is
  guessed, so the box never gets a variable nothing reads. `edit --env` changes it.
- `delete` asks for confirmation; `-y` skips it.

## Details settled in review

A fresh reader of this document raised these. All are the author's calls, not the
user's; each is small enough to change in review.

- **Passthrough collision.** `env::compose` already bails when a secret's source
  variable is also passthrough or `-e`. For a store secret the in-box env name
  plays that role, so it is passed in as an excluded name. A host variable with
  that name therefore fails `up` with the existing message rather than reaching
  the box. This is a small change to the `up` wiring, not to `compose` itself.
- **Resume warning.** `sidecar::hash_secret_value` hashes only the value, so
  editing hosts or the env name would go unnoticed on resume. For store-backed
  secrets the hash covers value, hosts and env name. Flag-form secrets keep the
  value-only hash, so existing boxes do not warn spuriously.
- **Names.** Lowercase letters, digits, `-` and `_`, starting with a letter (the
  name appears inside the `<BOXLITE_SECRET:name>` placeholder). `gh` stays
  reserved. A bare `--secret NAME` that is not in the store fails `up`; it never
  falls back to a host env var. Two secrets mapping to the same in-box env name
  in one `up` is an error.
- **Create and delete.** `create` on a name that already exists errors and points
  at `edit`. It checks the store entry, not the index, so an orphan entry without
  an index line reports "exists, run `delete`". Order: write the store entry,
  then the index. `delete` removes the store entry, then the name, and succeeds
  if either is already gone.
- **Value input.** Strip one trailing newline from piped input; reject an empty
  value. `edit` with only host or env flags does not prompt for a value; `edit`
  with no flags, or with piped input, replaces the value.
- **Other platforms.** The macOS store crate is behind `cfg(target_os = "macos")`
  and CI keeps building on Linux. Without a store, `secret` commands and a bare
  `--secret NAME` fail with the same "no secret store available" error.
- **WSL store.** Copied from the spike into a module in `cbox/`, with the
  PowerShell script embedded using `include_str!` and written to a private
  temporary directory per run.

## Left to the builder

Exact module layout, crate versions, the JSON field names, clap wiring, the
hidden-prompt implementation, and tests (including a mock store so tests don't
touch a real Keychain).
