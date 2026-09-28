# `--allow-net` / `--network disabled` — design

Status: approved in brainstorming, not yet implemented. Tracks item 2 of issue #39.

## Intent

**Goal: stop exfiltration.** A prompt-injected agent or a poisoned dependency running in the
box should not be able to ship source or credentials to arbitrary hosts. Losing convenience
(e.g. `WebFetch`) is an accepted cost when a box is locked down.

**Shape: per-use, opt-in.** Each box is opened only to what that use needs, chosen when it is
created. With no network flags a box stays fully open, exactly as today — no existing workflow
changes. The host-side gateway is always reachable from a restricted box, since Claude cannot
run without it.

## What BoxLite 0.10.4 actually provides (read from source)

- `BoxOptions.network: NetworkSpec` — `Enabled { allow_net: Vec<String> }` or `Disabled`
  (`runtime/options.rs:971-981`). Empty `allow_net` = unrestricted; non-empty = only listed
  destinations; `Disabled` = no network backend, no `eth0` in the guest.
- **Matching** (`net/gvproxy/config.rs:85-89`): IP and CIDR rules match the destination address;
  hostname rules match the peeked TLS SNI / HTTP `Host` and are dialed by name by the gateway.
  **DNS is not filtered.** This contradicts `cbox.md`'s "DNS-sinkholes everything else", which
  must be corrected.
- **Immutable after creation.** There is no API to change `allow_net` on an existing box.
- **Recorded per box.** `LiteBox::info()` → `BoxInfo.network: Option<NetworkInfo>`, whose
  `outbound.allow_net` / `outbound.mode` is BoxLite's own record of the policy
  (`runtime/types.rs:329-400`). Drift detection reads this; nothing new goes in the sidecar.
- Secret hosts are **not** auto-allowed: `--secret`/the GitHub preset does not open GitHub's
  hosts by itself.

Consequences that shape the design and must be documented:

1. DNS remains a low-bandwidth exfiltration channel. An allow-list narrows egress; it does not
   make exfiltration impossible.
2. Hostname rules only work for HTTP(S). SSH (e.g. `github.com:22`) carries no SNI/Host, so it
   needs an IP/CIDR rule.
3. Every allowed host is an upload channel. `@github` means *all of GitHub*, including pushing
   to an attacker's repo with an attacker-supplied token. Host granularity cannot express
   "this repo/org" (see Out of scope).
4. Claude reaches the gateway over plain HTTP at `host.boxlite.internal` (`192.168.127.254`).
   Allowing that address has no port syntax, so it re-opens **every** published host port.
   `docs/design/agentgateway.md`'s ports policy stays exactly as load-bearing as written.

## CLI

On `cbox up` only (network policy is a creation-time property; `exec` is unaffected):

- `--allow-net <RULE>`, repeatable. `RULE` is one of:
  - `@preset` — a named bundle (below);
  - an exact host (`api.example.com`), `*.domain`, an IP, or a CIDR — passed to BoxLite as-is.
- `--network disabled` — no network at all. Mutually exclusive with `--allow-net` (clap
  `conflicts_with`).

`just up` / `up-dev` already forward arguments to `cbox up`; no justfile change.

## Policy resolution — `cbox/src/netpolicy.rs`

Pure logic, no BoxLite runtime calls, fully unit-testable.

```rust
pub enum Policy { Open, Allow(Vec<String>), Disabled }
```

- `PRESETS`: static table `name -> &[&str]`. Changes to it go through code review and a
  release, and no file a box can reach can widen it.
- `resolve(rules: &[String], disabled: bool) -> Result<Policy>`:
  - no rules and not disabled → `Open`;
  - `disabled` → `Disabled`;
  - otherwise expand presets, validate raw rules, add the gateway IP `192.168.127.254`, dedupe,
    sort → `Allow(list)`.
  - Unknown `@name` → error listing valid presets. Malformed rule (empty, whitespace, `*`
    anywhere other than a leading `*.`) → error.
- The gateway is added by **IP**, not hostname: gateway traffic is plain HTTP, and IP matching
  does not depend on the `Host` header.
- `Policy::to_spec() -> NetworkSpec`.
- `Policy::from_info(Option<&NetworkInfo>) -> Recorded` where `Recorded` is a `Policy` or
  `Unknown`. `Enabled` with an empty list reads as `Open`; a missing `NetworkInfo` reads as
  `Unknown`, which never compares equal to a requested policy.
- Equality between policies is order- and duplicate-insensitive.

### Presets (initial; audited against real traffic before merge, see Verification)

| Preset     | Hosts |
|------------|-------|
| `@github`  | `github.com`, `api.github.com`, `codeload.github.com`, `*.githubusercontent.com`, `uploads.github.com` |
| `@npm`     | `registry.npmjs.org` |
| `@crates`  | `crates.io`, `index.crates.io`, `static.crates.io` |
| `@pypi`    | `pypi.org`, `files.pythonhosted.org` |
| `@debian`  | `deb.debian.org`, `security.debian.org` |

## Wiring

- `boxopts::UpFlags` gains `network: Policy`; `boxopts::build` sets
  `network: flags.network.to_spec()`.
- `main.rs` gets only the clap definitions and passes them through `UpArgs`, per its
  "clap surface and dispatch only" rule.

## Output

When the effective policy is not `Open`, `up` prints:

```
cbox: egress restricted to: 192.168.127.254 (gateway, implicit), api.github.com, github.com, ...
cbox: note: the gateway IP opens every published host port, not just :15002/:15003 (see docs/design/agentgateway.md)
```

For `Disabled`: `cbox: network disabled`, plus a warning when the command is `claude` that it
cannot reach the Anthropic API or MCP through the gateway.

## Reuse and drift

`get_or_create` ignores the options of an existing box, so a requested policy could otherwise
be silently dropped — a security control that appears applied but is not. After
`get_or_create` returns an existing box (`created == false`), and before `start()`:

| Requested | Recorded vs requested | Behaviour |
|-----------|-----------------------|-----------|
| `Open` (no flags) | any | Resume. If the box is restricted/disabled, print its recorded policy (`cbox: box <name> egress restricted to: …`). Lifting a restriction requires `-f`. |
| non-`Open` | equal | Resume; print the policy. |
| non-`Open` | differs, or recorded `Unknown` | Prompt (below). |
| any | — with `-f` | No prompt; the box was already removed before `get_or_create`, as today. |

Prompt, on stderr:

```
cbox: box <name> was created with egress: <recorded policy>
cbox: you asked for:                  <requested policy>
cbox: its network policy can't be changed without recreating it.
  [r] recreate with the new policy (discards the box's disk and state; ends its running sessions)
  [c] continue with the box as is
  [a] abort (default)
```

- The "ends its running sessions" clause appears only when the box is `Running` (detached, or
  another `up` attached).
- `r` → `runtime.remove(name, true)`, then `runtime.create(options, name)`, then continue down
  the same path as a freshly created box (sidecar written with current secret hashes). No
  separate recreate code path.
- `c` → continue with the existing box; print `cbox: continuing with existing policy: …` so the
  choice is visible in scrollback.
- `a`, Enter, EOF, or any other input → exit non-zero, box untouched.
- **No TTY** (stdin or stderr not a terminal): never prompt. Abort, printing the two policy
  lines and `rerun with -f/--force to recreate`. Fail closed.

The prompt reads one line with a blocking `read_line` on `spawn_blocking`, before `attach`
enters raw mode. It always terminates on a line or EOF, so it does not reintroduce the
uncancellable-`tokio::io::stdin` shutdown hang `stdin_reader.rs` exists to avoid. Prompting sits
behind a small `trait Prompter` so the decision table is unit-testable without a terminal.

Also: `reuse_message` adds "network policy" to the list of settings that date from creation,
and the `get_or_create` comment in `up.rs` points at this check.

## Documentation changes

- `docs/design/cbox.md`, "Egress allow-list":
  - replace "DNS-sinkholes everything else" with the matching model above, tagged
    **read from source** until measured, then **verified** with the measurement inline;
  - document presets, the implicit gateway IP, the reuse prompt, `--network disabled`;
  - document the limits (DNS channel, SSH needs IP/CIDR, every allowed host is an upload
    channel, `@github` = all of GitHub; recommend fine-grained PATs scoped to specific
    repos/orgs meanwhile);
  - pair it with the untested secret-scoping open question: each bounds the other's failure,
    neither substitutes for the other;
  - mark `--allow-net` / `--network disabled` as shipped in the phase header.
- `docs/design/agentgateway.md`: one cross-reference — `--allow-net` re-opens every published
  host port via the gateway IP; the ports policy is unchanged.
- `AGENTS.md`: add both flags to the `up` flag list.
- Issue #39: tick item 2 and suggestions 1/5 (for network flags) once merged.

## Testing

Unit (`cargo test`, no VM):

- `netpolicy`: preset expansion; unknown preset error lists valid names; rule validation;
  gateway added only to non-empty lists; dedupe/sort; `Open`/`Allow`/`Disabled` ↔ `NetworkSpec`
  round trip; empty `Enabled` = `Open`; missing `NetworkInfo` = `Unknown`; order-insensitive
  equality.
- Drift decision table through a fake `Prompter`: every row above; inputs `r`, `c`, `a`, Enter,
  EOF, garbage; no TTY → abort; `-f` never prompts.
- `boxopts::build` threads the policy into `BoxOptions.network`.
- clap rejects `--allow-net` together with `--network disabled`.

`cargo build --release` stays at zero warnings.

## Verification (live, isolated `BOXLITE_HOME`, cleaned up afterwards)

Requires a real host; cannot run in a cloud container. Doc tags stay **read from source** until
these pass.

1. `--allow-net @github`: Claude starts and MCP works (gateway IP passes); `gh api user` and
   HTTPS `git clone` succeed; `curl https://example.com` is refused; `dig example.com` resolves
   (confirms DNS is unfiltered).
2. `--network disabled -- bash`: no `eth0`; warning printed.
3. Reuse: create open, then `up --allow-net @npm` → prompt; `r`, `c`, `a` each behave as
   specified; `</dev/null` aborts.
4. Preset audit: `npm install`, `cargo fetch`, `pip download`, `apt-get update` under their
   presets; add any missing hosts before merge.

## Delivery

One PR to `main`: `feat(cbox): add --allow-net egress allow-list and --network disabled`.

## Out of scope

- **Per-repo / per-org GitHub access.** Not expressible at host granularity. The real control is
  routing git/API traffic through the host-side agentgateway with path rules
  (`/org/repo.git/...`, `/repos/org/...`) and gateway-side token injection, removing GitHub
  from the box's allow-list. Open problems: `gh`'s GraphQL (`POST /graphql`) carries the repo in
  the body, and the GitHub MCP server needs tool-argument policy. Gateway capability is
  unverified. Separate brainstorm/spec.
- User-defined presets or a preset config file.
- Showing the policy in `cbox list` / `inspect` (belongs to #39's observability item).
- Changing the default from open to restricted.
