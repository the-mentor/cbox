# cbox secrets: design

Status: approved by the user on 2026-11-06. PRs 1 to 4 (store, `up` integration, WSL store, `edit`) are implemented; the Linux desktop store is not (see the tracking issues).

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
- cbox itself never writes a secret value to disk. The OS store holds the value
  encrypted, and the secrets index holds names only. BoxLite keeps a copy of the
  values of the secrets a box was created with in that box's own database under
  the box home (not encrypted, protected by file permissions; cbox makes the box
  home owner-only) until the box is removed (`cbox down`, or `cbox up -f`). Secrets
  from a host env var (the flag form) are kept there the same way.
  So `cbox secret delete` removes a secret from the store but does not revoke it
  for boxes that already used it: they keep the placeholder and BoxLite's copy of
  the value until they are removed.
- macOS: the Keychain (implemented). WSL: Windows Credential Manager
  (implemented; see the verification note under the WSL store below).
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

- Any path where cbox writes a secret value to disk unencrypted, including a
  fallback. (BoxLite's own per-box copy, described under "What is stored where",
  is written by BoxLite, not cbox, and goes away with the box.)
- A secret value on a command line (visible in `ps`, shell history).
- A command that prints a stored value.
- A profile or flag that can widen a secret's allowed hosts.

## What the user sees

```
$ cbox secret create openai --host api.openai.com --env OPENAI_API_KEY
Value for "openai" (input is hidden, press Enter when done): 
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
  drop empty entries and duplicates, as `parse_secret_flag` does today. An entry
  containing `/` or whitespace (e.g. a URL) is rejected: hosts are bare names.
- On `edit`, `--host` **replaces** the whole list; `--add-host` adds;
  `--remove-host` removes. `edit` prints the old and new list whenever hosts
  change. `edit` refuses to leave a secret with no hosts.
- `edit` ruling: if any of `--host`, `--add-host`, `--remove-host` or `--env` is
  given, `edit` never reads or prompts for a value and the value is unchanged.
  `edit NAME` with no flags replaces the value. `--host` conflicts with
  `--add-host` and `--remove-host` (clap error); the latter two may be combined.
  `edit` fails before any prompt or stdin read if the name is invalid, the
  entry is absent from the store (an index entry alone does not count) or
  unreadable.
  Every edit is a read-modify-write of the one entry, size-checked before `put`.
  After a successful `put`, `edit` adds the name to the index if it is missing
  (repairing a lost index); an index write failure reports the secret as updated.
- The value is entered at a hidden prompt, or read from stdin when piped
  (`pbpaste | cbox secret create openai --host api.openai.com --env OPENAI_API_KEY`).
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
| WSL | a small store written in cbox over Windows Credential Manager (implemented) |
| Linux desktop | `zbus-secret-service-keyring-store` (not implemented yet) |
| anything else | `cbox secret ...` fails with a clear "no secret store available" error |

**WSL store.** `WinCredStore` (`cbox/src/secretstore/wincred.rs`) implements `SecretStore`
directly, not a `keyring-core` store. It runs `powershell.exe` through WSL interop with an
embedded script (`wincred.ps1`) that calls `CredWriteW/CredReadW/CredDeleteW` via a compiled
C# type, under the target name `cbox/<name>`. It is used when `/proc/sys/kernel/osrelease`
contains "microsoft". The secret goes over stdin only, never argv. Each operation is one
PowerShell call of about 0.5 s (almost all startup). `list` and `up --secret NAME` read all
their names with one batched call (`get_many`; the names go over stdin and the script
re-validates each), so they cost one call per command; `create`, `edit` and `delete` take one
to three calls. If one entry fails with a Windows error other than not-found, `list` shows that
row as an error and the other rows normally, and `up` fails with an error naming that entry
(after one extra single read to get the cause). The script operations, including `getmany`
and the `!` marker, were checked on real Windows PowerShell 5.1 with a standalone script (19 checks,
not part of this repository). The Rust store was round-tripped by hand on WSL,
including batched `list` and `up` with one and with three secrets.
Credential Manager caps a value at 2560 bytes (fine for tokens). It needs WSL interop and
`powershell.exe` on PATH; if the program cannot be started, the error points at both.

**What is stored where.** One entry per secret in the OS store, under service
`cbox` with the secret name as the account (on WSL the Credential Manager target is `cbox/<name>`), whose stored value is a small JSON document holding the **value,
allowed hosts, and in-box env var name**. Keeping the hosts inside the protected
entry means anything able to widen a secret's hosts could equally read its value.
A separate plain file `~/.config/cbox/secrets.json` holds only the **list of
names** (so `list` can enumerate; `keyring-core` search is not implemented by
every store). If the index and the store disagree (entry removed in Keychain
Access, say), `list` shows the name as "missing value" and `delete` removes the
stale name. An entry that exists but cannot be read is listed as
`(unreadable: run cbox secret delete NAME)`, never with any of its content.

**BoxLite's copy.** BoxLite 0.11.0 serializes each `Secret`, value included, into
its per-box database under the box home, and keeps it there while the box exists
(its own doc on `Secret.value`: serialized "for DB persistence and shim config
pipe", protected by "DB file permissions"). That copy is not encrypted; cbox's
`secure_box_home` makes the box home owner-only (0700). It is removed with the box
(`cbox down`, or `cbox up -f`, which recreates it). Env-sourced secrets have always
been kept the same way.

**Index location.** The index path honours `$XDG_CONFIG_HOME` (`$XDG_CONFIG_HOME/cbox/secrets.json`, else
`~/.config/cbox/secrets.json`); unlike this, `registries.json` ignores XDG and uses `--config`,
`$CBOX_REGISTRIES`, then `~/.config/cbox/registries.json`.

**Use in `up`.** `--secret NAME` with no `=` and no `@` is a store lookup. The
form `NAME=ENV_VAR@hosts` is unchanged. A lookup builds the same `SecretSpec`
inputs `secrets::build` already consumes, with the value supplied by the store
instead of a host env var. A store-backed secret has no host source variable, so
its in-box env name is dropped from the passthrough list (the guest gets the
placeholder, and a host variable of that name is not forwarded), and is still added
to the exclusion list `env::compose` takes as `secret_vars`, so a `-e` flag of the
same name makes `up` fail instead of leaking a value into the box (see "Details
settled in review").
The resume warning needs a small change for store-backed secrets (same section).

**Size limit and edits.** Windows Credential Manager accepts at most 2560 bytes per
entry (measured by spike: 2560 saves and reads back, 2561 fails with the unclear
"The stub received bad data"). The stored JSON counts toward it, in bytes, not
characters. `create` and `edit` measure the serialized document and, on WSL, fail
with a clear "too large" message before writing anything. Every `edit` is a
read-modify-write of the single entry (value read internally, never printed), so
hosts, env name and value cannot drift apart. If a bigger secret is ever needed,
the escape hatches are splitting into two entries or keeping only a key in the
store with an encrypted file; neither is in scope.

## PR plan

Merge order matters only as noted: 1 before 2; 3 any time after 1.

1. **Store interface, macOS backend, `secret create | list | delete`.** On
   other platforms the commands fail with "no secret store available on this platform ...".
2. **`up` integration.** `--secret NAME` resolves from the store.
3. **WSL backend.** Detection, actionable errors. Implemented.
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

- **Passthrough collision.** `env::compose` bails when a secret's source
  variable is also passthrough or `-e`. That stays true for the flag form. For a
  store secret the value does not come from the host, so `up` removes its in-box
  env name from the passthrough list before `compose` (this matters because
  names like `ANTHROPIC_API_KEY` are on that hardcoded list): the box gets the
  placeholder and the host variable of that name is not forwarded. The name is
  still passed in as an excluded name, so `-e NAME=...` fails `up` with the
  existing message. This is a change to the `up` wiring, not to `compose` itself.
- **Resume warning.** `sidecar::hash_secret_value` hashes only the value, so
  editing hosts or the env name would go unnoticed on resume. For store-backed
  secrets the hash covers value, hosts and env name. Flag-form secrets keep the
  value-only hash, so existing boxes do not warn spuriously.
- **Names.** Lowercase letters, digits, `-` and `_`, starting with a letter (the
  name appears inside the `<BOXLITE_SECRET:name>` placeholder). `gh` and `gh_basic` (the
  two built-in GitHub secrets) stay reserved, in both the store and the flag form. A bare `--secret NAME` that is not in the store fails `up`; it never
  falls back to a host env var (confirmed by the user). Repeating a `--secret`
  name, or two secrets sharing an in-box env var in one `up`, is an error; this
  is now also the case for the flag form (the GitHub preset is reported as "the
  GitHub token (GH_TOKEN/GITHUB_TOKEN)").
- **Create and delete.** `create` on a name that already exists errors and points
  at `edit`. It checks the store entry, not the index, so an orphan entry without
  an index line reports "exists but not in the index" and offers `edit` (keeps it)
  or `delete`. Order: write the store entry,
  then the index; if the index cannot be written, `create` says the secret was
  stored and points at `cbox secret edit x` to add it to the index. `delete`
  loads the index first (a corrupt index fails before the store is touched),
  removes the store entry, then the name, and succeeds if either is already
  gone; if neither exists it fails with `secret "x" not found`. If the index
  cannot be written afterwards it says the secret was deleted. Existence is
  probed without parsing the entry, so a corrupt entry can still be deleted, and
  `create` on one (indexed or not) reports `secret "x" exists in <store> but
  cannot be read; run `cbox secret delete x` and create it again`.
- **Value input.** Strip one trailing newline from piped input; reject an empty
  value. `edit` with only host or env flags does not prompt for a value; `edit`
  with no flags replaces the value (hidden prompt or piped stdin); with any flag the value is never read.
- **Other platforms.** The macOS store crate is behind `cfg(target_os = "macos")`
  and CI keeps building on Linux. Without a store, `secret` commands and a bare
  `--secret NAME` fail with the same "no secret store available" error.
- **WSL store.** **The PowerShell
  script is never written to disk** (decided by the user, to stop anyone swapping
  it to capture values): it is embedded in the binary with `include_str!` and
  passed to `powershell.exe -NoProfile -NonInteractive -EncodedCommand <base64
  UTF-16LE>`. The operation and target name are prepended as variable
  assignments; the target is safe to inline because names are restricted to
  lowercase letters, digits, `-` and `_` (and `encode_command` re-checks it). The secret still travels over stdin
  only. This removes the temporary file, `wslpath`, the UNC script path and
  `-ExecutionPolicy Bypass`. The same script and flags were verified on Windows
  PowerShell 5.1 under WSL by a standalone script (stdin free, UTF-8, not-found as exit code 3, other failures as exit code 1, long command line); the Rust store was then round-tripped by hand on WSL (create hidden and piped, duplicate, list,
  edit with every flag and value-only, not-found for get, edit, delete and `up`, the size limit,
  `up --secret` placeholder). Not verified: a non-English locale, WSL interop off, PowerShell blocked by
  policy or Constrained Language Mode, a hung PowerShell (there is no timeout). This protects against
  tampering with cbox's script, not against other software running as the same
  Windows user, which can call Credential Manager directly (it has no per-app
  access control).

## Left to the builder

Exact module layout, crate versions, the JSON field names, clap wiring, the
hidden-prompt implementation, and tests (including a mock store so tests don't
touch a real Keychain).
