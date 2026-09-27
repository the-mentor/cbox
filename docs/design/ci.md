# CI and releases

## Why this exists

Every `just up`/`exec`/`down`/`list` recipe shells out to the `cbox` binary (`cbox_bin` in
`justfile`). Building it (`just build-cbox`) needs a Rust toolchain and `protoc >= 3.12`, because
`boxlite`'s `build.rs` compiles `boxlite-shared` from source. CI builds and tests `cbox` on every
PR, and publishes prebuilt binaries on each release, so `just install-cbox` can fetch one instead.
`build-cbox` stays the path for anyone editing `cbox`'s source, since a published binary can never
reflect local changes.

There are two workflows. `ci.yml` builds and tests. `release.yml` cuts releases, and runs only
after `ci` passes on a merge to `main`:

```
merge to main ──> ci (changes → build → CI) ──success──> release (release-please → assets)
```

## `.github/workflows/ci.yml`

Triggers: `pull_request` (any base) and `push` to `main`. The push run keeps the cache warm for
PRs (see Caching).

### `changes` → `build` → `CI`

`changes` diffs the PR against its base, or a push against `github.event.before`. `build` runs
only if something under `cbox/`, `ci.yml` itself, or `CHANGELOG.md` changed. `CHANGELOG.md` only
changes in release PRs, whose merge always needs binaries (see `assets`). With no usable base (under act, or
the first push of a branch), `changes` answers "yes" and `build` runs.

`CI` is a single gate job that always runs. It passes when `changes` succeeded and `build` either
passed or was skipped. **The ruleset should require `CI`, not the per-row `build (...)` checks.**
A matrix job skipped by `if:` never expands its matrix, so its row names never report, and a
docs-only PR would block forever. Requiring `CI` also means adding or dropping a matrix row needs
no ruleset change.


A matrix of two GitHub-hosted runners, no cross-compiling:

| OS | Runner | Asset name |
| --- | --- | --- |
| Linux x86_64 | `ubuntu-latest` | `cbox-linux-x86_64` |
| macOS arm64 | `macos-14` | `cbox-macos-arm64` |

It installs `protoc`, then installs and selects the pinned Rust version, `RUST_VERSION` (see
Toolchain pinning). Both runners ship rustup; under act it installs rustup first. It then restores the cargo cache, runs `Unit Test` (`cargo test --release`) and
`cargo build --release`, and uploads the binary as an artifact named after its asset.

### Caching

Uncached, `Unit Test` takes about 3.5 minutes on both runners, nearly all of it compiling
dependencies (`boxlite` and friends). The build after it takes about 5s because it reuses the
test profile's output. `actions/cache` saves `~/.cargo/registry/{index,cache}`, `~/.cargo/git/db`
and `cbox/target`, keyed on `runner.os`, `RUST_VERSION` and the hash of `cbox/Cargo.lock`.
`restore-keys` falls back to the newest cache for that OS and Rust version, so a lockfile bump
still reuses most of the compiled dependencies. Artifacts built by one compiler version can't be
reused by another, which is why the Rust version is in the key and the fallback. `CARGO_INCREMENTAL=0` keeps incremental-compilation data, which a fresh runner can't
use, out of the cache.

GitHub scopes caches by branch. A PR can restore caches saved on its base branch (`main`), but not
caches from other PRs. That is why `ci.yml` also runs on push to `main`: those runs save the
caches that every PR starts from. A cache hit on an exact key doesn't re-save. That is fine,
because the key only changes when `Cargo.lock` or `RUST_VERSION` does. If caches get stale or bloated, delete them
with `gh cache delete --all`.

`Swatinem/rust-cache` would also prune stale artifacts, but it is not a verified-creator
publisher, which is the bar in Action pinning below.

**The `CI` job name is load-bearing.** The ruleset requires it by name, so renaming it means
updating the ruleset in the same change, or every PR blocks on a check that never reports.

The Linux binary links dynamically against whatever glibc `ubuntu-latest` ships (Ubuntu 24.04 →
glibc 2.39), so it won't run on older distros. Pin an older runner if a lower floor is needed.

The macOS binary is not explicitly codesigned. The linker's ad-hoc signature was enough for
`boxlite`'s macOS backend on the one Apple Silicon machine tested. If a released binary fails
there on another Mac, add a `codesign --sign - --entitlements ...` step at that point.

## `.github/workflows/release.yml`

Trigger: `workflow_run` on `ci` completing for `main`. Both jobs are skipped unless that ci run
succeeded and was itself triggered by a push. The ruleset only lets changes into `main` through
PRs, so that push is always a merge. The push check matters because `branches: [main]` matches the
run's head branch name, which a fork PR can also call `main`.

This makes a release depend on ci passing for the exact commit being released, not just on the
PR's checks. The PR checks can pass while the merged result fails, because the ruleset doesn't
require PRs to be up to date. `concurrency: release` queues back-to-back runs instead of running
them in parallel.

`workflow_run` always uses the copy of `release.yml` on the default branch. A PR that edits it
can't test the change before merging.

### `release-please`

This follows the same flow as
[the-mentor/no-ai-attribution](https://github.com/the-mentor/no-ai-attribution):

1. It mints a token for the release GitHub App (`vars.RELEASE_APP_ID`, `secrets.RELEASE_APP_KEY`)
   with `actions/create-github-app-token`. PRs and merges made with the app's token trigger
   workflows, which `GITHUB_TOKEN`'s don't. That matters because the release PR needs its
   required `build` checks to run.
2. `googleapis/release-please-action` reads Conventional Commits since the last release. It then
   opens or updates a release PR, and when that PR merges, the next run tags `vX.Y.Z` and
   publishes the GitHub Release. **There is one release for the whole repo, not one per
   component.** The config (`release-please-config.json`) uses the `simple` type on the `.`
   package, so the PR bumps the root `CHANGELOG.md` and `.release-please-manifest.json`. It also
   bumps cbox's version in `cbox/Cargo.toml` and `cbox/Cargo.lock` through `extra-files`, which
   keeps `cbox --version` matching the tag. Artifacts added later, such as the container images,
   should publish under the same tag. Any version string they carry goes in `extra-files` and in
   the guard's allowlist.

   The `Cargo.lock` jsonpath is `$.package[?(@.name.value=='cbox')].version`, not
   `@.name=='cbox'`. release-please's TOML parser wraps every value in `{start, end, value}`, so
   the shorter filter silently matches nothing.
3. **Auto-merge guard.** When the step opens or updates the PR, the job merges it itself, but
   only after two checks pass. First, the PR's author must be this app. Second,
   `.github/scripts/check_release_pr.py` must confirm the diff touches only those four files. It
   also checks that nothing changed in them except cbox's version, apart from entries prepended to
   the changelog. The
   merge is `--squash --admin --match-head-commit <checked sha>`, so a push after the check can't
   slip in. This relies on the app being a bypass actor on the default-branch ruleset.

Tags are `vX.Y.Z` (`include-component-in-tag: false`). While the version is below 1.0,
`bump-minor-pre-major` and `bump-patch-for-minor-pre-major` make `feat!` bump the minor version
and `feat` bump the patch version. `bootstrap-sha` points at the `main` commit this workflow
landed on, so the first changelog doesn't list the whole history.

### `assets` (only when a release was just created)

It doesn't build anything. It downloads the `cbox-*` artifacts from the ci run that triggered it
(`actions/download-artifact` with `run-id`, which needs `actions: read`), then runs `gh release
upload <tag> ... --clobber`. So the released binaries are the ones ci tested, from the same commit
and the same compiler.

Every release PR changes `CHANGELOG.md`, and `changes` treats that as a reason to build, so the
artifacts exist. `cbox/` alone isn't enough: a `Release-As` equal to the current version (which
is how `v0.1.0` was cut) bumps nothing in `cbox/Cargo.toml`, and that release shipped without
binaries until they were attached by hand.

**Edge case:** if a second merge lands before ci finishes for the release PR's merge, the later run
can be the one that creates the release. If that later ci run skipped the build, because it didn't
touch Rust, the download fails. Re-running the release workflow for the release PR's run fixes
it.

This is the only job with `contents: write`. It uses the plain `GITHUB_TOKEN`, because uploading
assets doesn't need to trigger anything.

## Toolchain pinning

`ci.yml` sets `RUST_VERSION` (currently `1.98.1`) at the workflow level and runs `rustup
toolchain install "$RUST_VERSION"` instead of tracking `stable`. Without the pin, every new stable
release (every six weeks) would:

- invalidate the cache: the key stays the same, but cargo can't reuse artifacts from the old
  compiler, so every run recompiles all dependencies and never re-saves the cache;
- let CI fail on commits that don't touch Rust, through new lints or warnings.

Release binaries come from ci's artifacts, so the pin covers them too. There's no `rust-toolchain.toml`, so local
`just build-cbox` keeps using whatever toolchain the developer has.

**Bumping it:** change `RUST_VERSION` in `ci.yml`. The cache
key changes with it, so the first run afterwards is a cold build. Dependabot doesn't track this
value; bump it by hand.

## Running CI locally: `just ci-local`

`just ci-local` runs the `build` job's `ubuntu-latest` row under
[nektos/act](https://github.com/nektos/act). The repo's `.actrc` maps `ubuntu-latest` to
`catthehacker/ubuntu:act-latest` and enables act's local artifact server, so the upload step
works. It needs Docker and `act` on the host (`brew install act`). act only runs Linux containers,
so the macOS row can't run locally. `release.yml` is never run locally, because it needs the
release app's credentials and would publish for real.

## Action pinning

Every `uses:` is pinned to a full commit SHA, with its version as a trailing comment. The only
publishers are the `actions` org and `googleapis` (release-please). `.github/dependabot.yml`
bumps the pins weekly, grouped into a single PR.

## Local install: `just install-cbox [tag]`

It picks `cbox-linux-x86_64` or `cbox-macos-arm64` from `uname`, and fails on anything else. It
downloads from `releases/latest/download/<asset>`, or from `releases/download/<tag>/<asset>` when
a tag is given, into `cbox_bin`'s path, writing through a temp file plus `mv`. `curl -f` makes a
missing release fail with a 404 instead of writing an HTML page as the binary.
