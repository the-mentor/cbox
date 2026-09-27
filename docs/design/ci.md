# cbox CI and releases

## Why this exists

Every `just up`/`exec`/`down`/`list` recipe shells out to the `cbox` binary (`cbox_bin` in
`justfile`). Building it (`just build-cbox`) needs a Rust toolchain and `protoc >= 3.12`, because
`boxlite`'s `build.rs` compiles `boxlite-shared` from source. CI builds and tests `cbox` on every
PR, and publishes prebuilt binaries on each release, so `just install-cbox` can fetch one instead.
`build-cbox` stays the path for anyone editing `cbox`'s source, since a published binary can never
reflect local changes.

## Workflow: `.github/workflows/cbox.yml`

Triggers: `pull_request` (any base) and `push` to `main`.

### `build` (every run)

A matrix of two GitHub-hosted runners, no cross-compiling:

| OS | Runner | Asset name |
| --- | --- | --- |
| Linux x86_64 | `ubuntu-latest` | `cbox-linux-x86_64` |
| macOS arm64 | `macos-14` | `cbox-macos-arm64` |

It installs `protoc`, runs `rustup update stable` (both runners ship rustup; under act it installs
rustup first), then `cargo test --release` and `cargo build --release`, and uploads the binary as
an artifact named after its asset.

**The job and matrix names are load-bearing.** The default-branch ruleset requires the checks
`build (ubuntu-latest, cbox-linux-x86_64)` and `build (macos-14, cbox-macos-arm64)` by name.
Renaming the job or a matrix value means updating the ruleset in the same change, or every PR
will block on a check that never reports.

The Linux binary links dynamically against whatever glibc `ubuntu-latest` ships (Ubuntu 24.04 →
glibc 2.39), so it won't run on older distros. Pin an older runner if a lower floor is needed.

The macOS binary is not explicitly codesigned. The linker's ad-hoc signature was enough for
`boxlite`'s macOS backend on the one Apple Silicon machine tested. If a released binary fails
there on another Mac, add a `codesign --sign - --entitlements ...` step at that point.

### `release-please` (push to `main` only, needs `build`)

This follows the same flow as
[the-mentor/no-ai-attribution](https://github.com/the-mentor/no-ai-attribution):

1. It mints a token for the release GitHub App (`vars.RELEASE_APP_ID`, `secrets.RELEASE_APP_KEY`)
   with `actions/create-github-app-token`. PRs and merges made with the app's token trigger
   workflows, which `GITHUB_TOKEN`'s don't. That matters because the release PR needs its
   required `build` checks to run.
2. `googleapis/release-please-action` reads Conventional Commits since the last release. It then
   opens or updates a release PR that bumps `cbox/Cargo.toml`, `cbox/Cargo.lock`,
   `cbox/CHANGELOG.md` and `.release-please-manifest.json`, using release-please's `rust` release
   type (config in `release-please-config.json`). When that PR merges, the next run tags `vX.Y.Z`
   and publishes the GitHub Release.
3. **Auto-merge guard.** When the step opens or updates the PR, the job merges it itself, but
   only after two checks pass. First, the PR's author must be this app. Second,
   `.github/scripts/check_release_pr.py` must confirm the diff touches only those four files and
   changes nothing in them except cbox's version, plus entries prepended to the changelog. The
   merge is `--squash --admin --match-head-commit <checked sha>`, so a push after the check can't
   slip in. This relies on the app being a bypass actor on the default-branch ruleset.

Tags are `vX.Y.Z` (`include-component-in-tag: false`). While the version is below 1.0,
`bump-minor-pre-major` and `bump-patch-for-minor-pre-major` make `feat!` bump the minor version
and `feat` bump the patch version. `bootstrap-sha` points at the `main` commit this workflow
landed on, so the first changelog doesn't list the whole history.

### `upload-assets` (only when a release was just created)

It downloads both `build` artifacts from the same run and runs `gh release upload <tag> ...
--clobber`. It is the only job with `contents: write`, and it uses the plain `GITHUB_TOKEN`
because uploading assets doesn't need to trigger anything.

## Running CI locally: `just ci-local`

`just ci-local` runs the `build` job's `ubuntu-latest` row under
[nektos/act](https://github.com/nektos/act). The repo's `.actrc` maps `ubuntu-latest` to
`catthehacker/ubuntu:act-latest` and enables act's local artifact server, so the upload step
works. It needs Docker and `act` on the host (`brew install act`). act only runs Linux containers,
so the macOS row can't run locally. The `release-please` and `upload-assets` jobs only run on
push, so they never run under `ci-local`'s `pull_request` event.

## Action pinning

Every `uses:` is pinned to a full commit SHA, with its version as a trailing comment. The only
publishers are the `actions` org and `googleapis` (release-please). `.github/dependabot.yml`
bumps the pins weekly, grouped into a single PR.

## Local install: `just install-cbox [tag]`

It picks `cbox-linux-x86_64` or `cbox-macos-arm64` from `uname`, and fails on anything else. It
downloads from `releases/latest/download/<asset>`, or from `releases/download/<tag>/<asset>` when
a tag is given, into `cbox_bin`'s path, writing through a temp file plus `mv`. `curl -f` makes a
missing release fail with a 404 instead of writing an HTML page as the binary.
