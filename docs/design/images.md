# Base image: build and publish

This covers `.github/workflows/base-image.yml` and how it's called from `ci.yml` and
`release.yml`, plus the local-build side in `custom/Dockerfile` and the `justfile`. For the
two-layer image itself (why it's split, what each layer bakes in), see `docs/design/general.md`.
This is not a change log — it doesn't track who did what or when, only what the setup is and why.

## Why

`base/` builds slowly (Debian + Node + Claude Code + `gh` + `uv`) and used to be a purely local
build — every machine that ran `just up-dev` paid that cost itself. It also goes stale: nothing
rebuilds it unless someone runs `just build-base` by hand, so the Claude Code version baked in
drifts further from upstream the longer a machine goes without a from-scratch rebuild. Publishing
`cbox-base` from CI fixes both: nobody builds the slow layer locally by default, and a weekly
rebuild keeps `:latest` no more than a week behind.

## What's published

Only `cbox-base`, to `ghcr.io/the-mentor/cbox-base` (public). `cbox-custom` stays per-person: it
bakes in per-repo config (plugins, `claude.json`) and is cheap to build, so there's nothing to
gain by publishing it, and every consumer building it themselves keeps its contents auditable
without trusting a shared image.

## Tags

| Tag | Written by | For |
| --- | --- | --- |
| `vX.Y.Z` | `release.yml`, after `release-please` cuts a release | pinning the exact base a release shipped with |
| `latest` | every release, and the weekly rebuild | what `custom/Dockerfile` and `just build` use by default |
| `YYYY-MM-DD` | the weekly scheduled rebuild | pinning a known-good base from before a bad upstream release, without waiting for the next `vX.Y.Z` |
| `pr-<n>` | `ci.yml`, for a same-repo PR that touches `base/` | trying that PR's base before merge, including in `custom-image`'s own build |

## `base-image.yml`

A reusable workflow with three triggers:
- **`workflow_call`**, from `ci.yml` and `release.yml`, with `tags`/`push`/`no-cache`/`ref` inputs
  and one output, `image` — the full reference of the first pushed tag, empty when nothing was
  pushed.
- **`schedule`**, weekly (`17 6 * * 1`, Monday ~06:00 UTC): builds `main`, pushes `latest` and
  today's date, cache skipped.
- **`workflow_dispatch`**: same as `schedule`, for re-running the weekly build by hand and for the
  very first publish.

The `build` job is a matrix of native runners — `linux/amd64` on `ubuntu-26.04`, `linux/arm64` on
`ubuntu-26.04-arm` — with no QEMU emulation. `.github/actionlint.yaml` lists both labels because
the installed `actionlint` predates them. QEMU-emulated arm64 builds are slow, and the runs that
matter most here — the uncached weekly refresh and every release — are exactly the ones that can't
afford that: they already pay for `no-cache`, so stacking emulation on top would make the slowest
runs the slowest by far.

Each matrix row pushes by digest (`push-by-digest=true`) rather than by tag, and uploads that
digest as an artifact. A separate `merge` job, which runs only when pushing, downloads both
digests and runs `docker buildx imagetools create` to attach the real tags to both digests at
once. No tag ever moves until both architectures exist — a run where one architecture fails never
leaves `:latest` (or a release tag) pointing at an amd64-only or arm64-only image. `merge` also
runs `imagetools inspect` on the first tag and fails the run unless both `amd64` and `arm64` show
up in the manifest list, so a bug in the merge step itself can't silently publish a single-arch tag.

The build cache (`type=gha`, scoped per architecture) is used on PR builds, and skipped
(`no-cache: true`) on the weekly and release builds. `base/`'s install steps — `npm install -g
@anthropic-ai/claude-code`, the oh-my-posh installer, `apt upgrade` — have fixed command text, so
Docker's cache would keep serving them from before even when upstream has moved on. PR builds
don't need the newest possible packages (that's what the next weekly rebuild or release is for),
so they keep the cache and stay fast; the two triggers whose whole point is to catch up with
upstream turn it off.

## Callers

`ci.yml`'s `base-image` job runs only on `pull_request`, when the `changes` job's `base` output is
true (paths under `base/`, or either workflow file). It always builds, but only pushes
`pr-<n>` when the PR is same-repo and not from Dependabot — fork and Dependabot tokens can't write
packages. `custom-image` then builds `custom/` on top: on `pr-<n>` when `base-image` pushed one,
otherwise on `:latest`. Both jobs feed into the `CI` gate alongside `build`, so a broken `base/` or
`custom/` blocks merge the same way a broken `cbox/` does.

`release.yml`'s `base-image` job runs after `release-please`, only when it just created a release,
tagging `<release tag>` and `latest`. It builds `ref: github.event.workflow_run.head_sha` — the
commit that was actually released — rather than `main`'s current tip, which may have moved on by
the time this job runs.

## Local builds

`custom/Dockerfile` starts `ARG BASE_IMAGE=ghcr.io/the-mentor/cbox-base:latest` /
`FROM ${BASE_IMAGE}`. The `justfile` exposes that arg as `CBOX_BASE_IMAGE` (default the same
`:latest`), so `just build`/`just build-image` build only `custom/`, on the published base,
pulling a fresh copy of it each time (`--pull`). That `--pull` is conditional on the base
reference containing a `/` — a registry reference like `ghcr.io/the-mentor/cbox-base:latest` — and
not applied to a bare local tag like `cbox-base`, because Docker would otherwise try to pull that
tag from Docker Hub instead of using the one just built locally.

`just build-local` is the pre-published flow: it runs `build-base` (building `base/` locally),
then `build-image` with `CBOX_BASE_IMAGE` forced to that local tag — it always uses the locally
built base, ignoring any `CBOX_BASE_IMAGE` already set in the environment. Reach for it when
changing `base/` itself, since a change there has nothing to test against until it's built.

`just build`/`just build-image` do honor an environment `CBOX_BASE_IMAGE`, so
`CBOX_BASE_IMAGE=ghcr.io/the-mentor/cbox-base:pr-67 just build` tries a PR's published base before
it merges.

## Errors and recovery

| Case | Behaviour |
| --- | --- |
| Weekly build fails | `:latest` is left alone, and GitHub emails the failure. Re-run through `workflow_dispatch`. |
| A bad upstream release gets baked into `:latest` | Pin a dated tag with `CBOX_BASE_IMAGE=ghcr.io/the-mentor/cbox-base:<date> just build`. |
| The release's image job fails | The release and binaries exist without `:vX.Y.Z`. Re-run the failed job. |
| One architecture fails | `merge` doesn't run, so no tag moves. The architecture that did build leaves an untagged image behind. |
| Fork or Dependabot PR | The base builds without pushing, and `custom-image` builds on `:latest`. |
| Weekly and release runs overlap | Both push `:latest`, and the last one wins. Both build commits on `main`. This is accepted. |
| Stale PR tags, dated tags, untagged images | Not cleaned up. A cleanup workflow can come later if storage matters. |

## One-time setup

GHCR creates a new package as private on its first push. After that first push, make `cbox-base`
public in the package's own settings, so pulls need no login and fork PRs can still build
`custom/` against it.

## Not covered

- Cleaning up old PR/dated/untagged tags.
- Publishing `cbox-custom`.
- Running the image jobs under `just ci-local` (it only runs `cbox`'s own Linux build).
