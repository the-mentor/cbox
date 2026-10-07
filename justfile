set dotenv-load

base_tag   := "cbox-base"
custom_tag := "cbox-custom"
# Base for custom/: the published multi-arch image by default. Override per
# command, e.g. CBOX_BASE_IMAGE=ghcr.io/the-mentor/cbox-base:pr-67 just build
# to try a PR's base, or use `just build-local` to build base/ locally.
base_image := env_var_or_default("CBOX_BASE_IMAGE", "ghcr.io/the-mentor/cbox-base:latest")
registry   := "localhost:5551"
compose    := "docker compose -f local-development/registry/docker-compose.yml"
# Appends agentgateway/docker-compose.override.yml when it exists: an untracked,
# per-machine file for local-only gateway changes (see README.md). Compose
# reads that name by itself only without -f, and this always passes -f.
gateway    := "docker compose -f agentgateway/docker-compose.yml" + `[ -f agentgateway/docker-compose.override.yml ] && echo " -f agentgateway/docker-compose.override.yml" || true`

default:
    @just --list

# Install the boxlite CLI itself (a prerequisite for this repo) by downloading
# the release tarball directly from GitHub (no curl|sh pipe) and verifying its
# sha256 checksum before installing. Installs the latest release by default;
# pass a version (e.g. v0.10.5) to pin. Installs into ~/bin by default; pass a
# directory to install elsewhere.
# Usage: just install-boxlite [version] [dir]
install-boxlite version="" dir=(env_var('HOME') + "/bin"):
    #!/usr/bin/env sh
    set -eu
    repo="boxlite-ai/boxlite"
    install_dir="{{dir}}"

    case "$(uname -s)-$(uname -m)" in
      Darwin-arm64) target="aarch64-apple-darwin" ;;
      Darwin-x86_64) echo "install-boxlite: macOS Intel is not supported; BoxLite requires Apple Silicon" >&2; exit 1 ;;
      Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
      Linux-aarch64|Linux-arm64) target="aarch64-unknown-linux-gnu" ;;
      *) echo "install-boxlite: unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
    esac

    fetch() {
      # $1 = url, $2 = output path
      if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
      elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
      else
        echo "install-boxlite: need curl or wget" >&2; exit 1
      fi
    }

    version="{{version}}"
    if [ -z "$version" ]; then
      echo "Resolving latest boxlite release..." >&2
      tmp_release="$(mktemp)"
      fetch "https://api.github.com/repos/${repo}/releases/latest" "$tmp_release"
      version="$(grep -m1 '"tag_name"' "$tmp_release" | sed -E 's/.*"tag_name":[[:space:]]*"([^"]+)".*/\1/')"
      rm -f "$tmp_release"
      [ -n "$version" ] || { echo "install-boxlite: could not resolve latest version" >&2; exit 1; }
    fi

    archive="boxlite-cli-${version}-${target}.tar.gz"
    base_url="https://github.com/${repo}/releases/download/${version}"

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    echo "Downloading ${archive} (${version})..." >&2
    fetch "${base_url}/${archive}" "${tmpdir}/${archive}"
    fetch "${base_url}/${archive}.sha256" "${tmpdir}/${archive}.sha256"

    expected="$(awk '{print $1}' "${tmpdir}/${archive}.sha256")"
    if command -v sha256sum >/dev/null 2>&1; then
      actual="$(sha256sum "${tmpdir}/${archive}" | awk '{print $1}')"
    else
      actual="$(shasum -a 256 "${tmpdir}/${archive}" | awk '{print $1}')"
    fi
    [ "$actual" = "$expected" ] || { echo "install-boxlite: checksum mismatch (expected $expected, got $actual)" >&2; exit 1; }

    mkdir -p "$install_dir"
    tar --no-same-owner -xzf "${tmpdir}/${archive}" -C "$tmpdir" boxlite
    install -m 0755 "${tmpdir}/boxlite" "${install_dir}/boxlite"
    echo "Installed ${install_dir}/boxlite (${version})" >&2
    case ":$PATH:" in
      *":${install_dir}:"*) : ;;
      *) echo "Note: ${install_dir} is not on your PATH. Add this to your shell rc file:" >&2
         echo "  export PATH=\"${install_dir}:\$PATH\"" >&2 ;;
    esac

# Symlink the cb wrapper (bin/cb) onto PATH so `cb up-dev` etc. work from
# any directory. Installs into ~/bin by default; pass a directory to
# install elsewhere.
# Usage: just install [dir]
install dir=(env_var('HOME') + "/bin"):
    #!/usr/bin/env sh
    set -eu
    mkdir -p "{{dir}}"
    ln -sf "{{justfile_directory()}}/bin/cb" "{{dir}}/cb"
    echo "Installed {{dir}}/cb -> {{justfile_directory()}}/bin/cb"
    case ":$PATH:" in
      *":{{dir}}:"*) : ;;
      *) echo "Note: {{dir}} is not on your PATH. Add this to your shell rc file:" >&2
         echo "  export PATH=\"{{dir}}:\$PATH\"" >&2 ;;
    esac

# Remove the symlink installed by `just install`.
# Usage: just uninstall [dir]
uninstall dir=(env_var('HOME') + "/bin"):
    rm -f "{{dir}}/cb"

# Start the local image registry (docker compose)
registry-up:
    {{compose}} up -d

# Stop the local image registry
registry-down:
    {{compose}} down

# Bootstraps agentgateway/htpasswd (gitignored, backs the admin UI's basic auth) from the
# tracked agentgateway/htpasswd.default template on first run only — if the
# live file already exists (e.g. a changed password), it is left alone, so a
# fresh clone gets a working default login with no setup step and a password
# change never leaves a tracked file modified. Not started by `just up`/`up-dev`.
# Start the host-side agentgateway (long-lived docker compose service).
gateway-up:
    #!/usr/bin/env sh
    set -eu
    [ -f agentgateway/htpasswd ] || cp agentgateway/htpasswd.default agentgateway/htpasswd
    {{gateway}} up -d

# Stop the host-side agentgateway
gateway-down:
    {{gateway}} down

# Follow the agentgateway logs
gateway-logs:
    {{gateway}} logs -f

# Change the password for the admin UI (on by default at 127.0.0.1:15000, see
# README.md "Admin UI"), overwriting agentgateway/htpasswd — the live,
# gitignored file `just gateway-up` bootstraps from the tracked
# agentgateway/htpasswd.default template — without touching that template, so
# changing the password never leaves a tracked file modified. Prompts for the
# password interactively via `htpasswd`/`openssl` themselves (hidden input,
# not echoed, never passed as an argument — that would land in both shell
# history and `ps` output). Prefers `htpasswd -B` (bcrypt, apache2-utils);
# falls back to `openssl passwd -apr1` if htpasswd isn't installed. See README.md "Admin UI".
# Change the password for the admin UI basic auth.
gateway-generate-ui-password username="admin":
    #!/usr/bin/env sh
    set -eu
    if command -v htpasswd >/dev/null 2>&1; then
      htpasswd -Bc agentgateway/htpasswd "{{username}}"
    elif command -v openssl >/dev/null 2>&1; then
      hash="$(openssl passwd -apr1)"
      printf '%s:%s\n' "{{username}}" "$hash" > agentgateway/htpasswd
    else
      echo "gateway-generate-ui-password: need htpasswd (apache2-utils) or openssl" >&2
      exit 1
    fi
    echo "Wrote agentgateway/htpasswd for user {{username}}" >&2

# Log in to an authenticated image registry (e.g. ECR) and store credentials in
# registries.local.json (gitignored). Mirrors `docker login`'s interface.
# Usage: aws ecr get-login-password --region <region> | just registry-login --registry <host> --username AWS --password-stdin
registry-login *args:
    ./scripts/registry-login.py {{args}}

# The four build recipes (build, build-image, build-base, build-local) forward extra
# arguments straight to `docker build`, so `--no-cache` (the reason this exists: Docker
# caches `RUN` layers by command text, so a `curl | sh` installer or an unpinned `npm i -g`
# keeps serving a stale version until the
# cache is bypassed) reaches whichever image is being built. `build`/`build-image` build only
# `custom/`, on the published base, so `just build --no-cache` rebuilds only that layer from
# scratch; `just build-local --no-cache` rebuilds base/ too. Any other `docker build` flag
# works the same way (e.g. `--pull`, `--progress=plain`).
# Usage: just build-base [docker-build-args...]
build-base *args:
    docker build {{args}} -t {{base_tag}} base/

# Build custom/ on top of base_image and push it to the local registry.
# --pull re-fetches a registry base (docker build never refreshes a base it
# already has, so :latest would go stale); a bare local tag like cbox-base is
# built as-is, since --pull would look for it on Docker Hub.
# CBOX_PRIVATE_MARKETPLACES/CBOX_PRIVATE_PLUGINS (in .env) bake in plugins from
# private repos. The token (GH_TOKEN, else `gh auth token`) goes to docker as a
# BuildKit secret read from the environment, never a build arg or argv, so
# it's in neither `docker history` nor `ps`. Unset = no gh, no token needed.
# Usage: just build-image [docker-build-args...]
build-image *args: registry-up
    #!/usr/bin/env bash
    set -euo pipefail
    private=()
    if [ -n "${CBOX_PRIVATE_MARKETPLACES:-}" ]; then
        export GH_TOKEN="${GH_TOKEN:-$(gh auth token 2>/dev/null || true)}"
        if [ -z "$GH_TOKEN" ]; then
            echo "CBOX_PRIVATE_MARKETPLACES is set but there's no GitHub token: set GH_TOKEN in .env or run gh auth login" >&2
            exit 1
        fi
        private=(--secret id=gh_token,env=GH_TOKEN
            --build-arg "PRIVATE_MARKETPLACES=$CBOX_PRIVATE_MARKETPLACES"
            --build-arg "PRIVATE_PLUGINS=${CBOX_PRIVATE_PLUGINS:-}")
    fi
    # ${private[@]+...}: macOS bash 3.2 treats an empty array as unset under set -u.
    docker build {{args}} {{ if base_image =~ '/' { "--pull" } else { "" } }} ${private[@]+"${private[@]}"} --build-arg BASE_IMAGE={{base_image}} -t {{custom_tag}} custom/
    docker tag {{custom_tag}} {{registry}}/library/{{custom_tag}}
    docker push {{registry}}/library/{{custom_tag}}
    just clean-cache

# Usage: just build [docker-build-args...]
build *args: (build-image args)

# Build base/ locally and custom/ on top of it: the pre-published flow, for
# changing base/ itself.
# Usage: just build-local [docker-build-args...]
build-local *args: (build-base args)
    CBOX_BASE_IMAGE={{base_tag}} just build-image {{args}}

# Build the cbox binary. Requires protoc >= 3.12 (brew install protobuf).
build-cbox:
    cd cbox && cargo build --release

# Fetch a prebuilt cbox binary from a GitHub Release instead of compiling it.
# Defaults to the latest release; pass a tag (e.g. v0.2.0) to pin one. Writes
# to the same path build-cbox does, so up/exec/down/list work unchanged.
# Usage: just install-cbox [tag]
install-cbox tag="":
    #!/usr/bin/env sh
    set -eu
    case "$(uname -s)-$(uname -m)" in
      Linux-x86_64) asset="cbox-linux-x86_64" ;;
      Darwin-arm64) asset="cbox-macos-arm64" ;;
      *) echo "install-cbox: no prebuilt binary for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
    esac
    if [ -n "{{tag}}" ]; then
      url="https://github.com/the-mentor/cbox/releases/download/{{tag}}/${asset}"
    else
      url="https://github.com/the-mentor/cbox/releases/latest/download/${asset}"
    fi
    out="{{cbox_bin}}"
    mkdir -p "$(dirname "$out")"
    tmp="$(mktemp "${out}.XXXXXX")"
    trap 'rm -f "$tmp"' EXIT
    echo "Fetching ${url}..." >&2
    curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp" "$url"
    chmod +x "$tmp"
    mv "$tmp" "$out"
    echo "Installed ${out}" >&2

# Run the cbox CI workflow's Linux build job locally with nektos/act (needs
# Docker). act can't run macOS jobs, so only the ubuntu-latest row runs.
# Extra args go to act, e.g. `just ci-local -l` to list jobs.
# Usage: just ci-local [act-args...]
ci-local *args:
    #!/usr/bin/env sh
    set -eu
    # act reads DOCKER_HOST but doesn't consult the docker CLI's context, so
    # non-default contexts (Rancher Desktop, Colima, ...) need it spelled out.
    # Assigning before export (rather than `export DOCKER_HOST="$(...)"`) means
    # a failed lookup trips `set -e` instead of silently exporting "".
    host="$(docker context inspect --format '{{"{{"}}.Endpoints.docker.Host{{"}}"}}')"
    export DOCKER_HOST="$host"
    act pull_request -W .github/workflows/ci.yml -j build --matrix os:ubuntu-latest {{args}}

# Refresh the custom image and sweep orphaned image blobs from boxlite's cache.
# BoxLite caches image tags immutably and has no `rmi`, so a rebuilt :latest is
# ignored until its cached tag->digest row is dropped; then the next
# `boxlite run` re-pulls from the registry. We also delete blob files
# (manifests/configs/layers/extracted) no longer referenced by any image in
# boxlite's index. Disk-images are left alone on purpose: boxlite does not
# record which image a disk-image belongs to, so an orphaned one can't be told
# apart from a live one without risking a costly (or breaking) re-pull.
clean-cache:
    #!/usr/bin/env sh
    set -eu
    command -v sqlite3 >/dev/null 2>&1 || { echo "clean-cache: sqlite3 not found; skipping" >&2; exit 0; }
    root="${BOXLITE_HOME:-$HOME/.boxlite}/boxes"
    [ -d "$root" ] || { echo "clean-cache: no box homes under $root; skipping" >&2; exit 0; }
    for dir in "$root"/*/; do
      [ -d "$dir" ] || continue
      home="${dir%/}"
      db="$home/db/boxlite.db"
      img="$home/images"
      [ -f "$db" ] || continue
      # Drop the custom tag so the next `boxlite run` re-pulls the pushed image.
      sqlite3 "$db" "DELETE FROM image_index WHERE reference='{{registry}}/library/{{custom_tag}}:latest';"
      # Blobs still referenced by any remaining image (filename form: sha256-...).
      keep="$(mktemp)"
      { sqlite3 "$db" "SELECT manifest_digest FROM image_index;"
        sqlite3 "$db" "SELECT config_digest FROM image_index;"
        sqlite3 "$db" "SELECT value FROM image_index, json_each(layers);"
      } | tr ':' '-' | sort -u > "$keep"
      for f in "$img"/manifests/* "$img"/configs/* "$img"/layers/* "$img"/extracted/*; do
        [ -e "$f" ] || continue
        key="$(basename "$f" | sed 's/\.json$//; s/\.tar\.gz$//')"
        grep -qx "$key" "$keep" || rm -rf "$f"
      done
      rm -f "$keep"
    done

# Build images and the cbox binary, then boot the box and launch Claude Code.
# Usage: just up-dev [box-name] [-f] [-c] [-v host:box ...] [-e KEY[=VALUE] ...] [-i image] [-d] [-- cmd...]
up-dev *args: build build-cbox (up args)

cbox_bin := justfile_directory() + "/cbox/target/release/cbox"

# Boot the box and launch Claude Code.
# Usage: just up [box-name] [-f] [-c] [-v host:box ...] [-e KEY[=VALUE] ...] [-i image] [-d] [-- cmd...]
#
# The `cd` is load-bearing: `just` sets the working directory to the justfile's
# own directory, so without it cbox would always see the repo root as its cwd
# and every derived box name would resolve to "cbox" no matter where
# the user was standing. Because the cd makes the path relative to the user
# instead, --config has to be passed explicitly.
up *args:
    #!/usr/bin/env sh
    set -eu
    [ -x "{{cbox_bin}}" ] || { echo "cbox binary not found at {{cbox_bin}} - run 'just build-cbox' or 'just install-cbox' first" >&2; exit 1; }
    # First-run bootstrap. This lived in the old `up` recipe; it has to stay
    # here rather than move into cbox, because cbox's cwd is now the user's
    # directory and it has no other way to find the repo's tracked template.
    [ -f "{{justfile_directory()}}/registries.local.json" ] || \
      cp "{{justfile_directory()}}/registries.json" \
         "{{justfile_directory()}}/registries.local.json"
    cd "{{invocation_directory()}}"
    exec "{{cbox_bin}}" up \
      --config "{{justfile_directory()}}/registries.local.json" {{args}}

# Open a session in the running box.
# Usage: just exec [box-name] [-- cmd...]
# Usage: just shell [box-name] [-- cmd...]
alias shell := exec
exec *args:
    #!/usr/bin/env sh
    set -eu
    [ -x "{{cbox_bin}}" ] || { echo "cbox binary not found at {{cbox_bin}} - run 'just build-cbox' or 'just install-cbox' first" >&2; exit 1; }
    cd "{{invocation_directory()}}"
    exec "{{cbox_bin}}" exec \
      --config "{{justfile_directory()}}/registries.local.json" {{args}}

# Stop and remove the box.
# Usage: just down [box-name]
down *args:
    #!/usr/bin/env sh
    set -eu
    [ -x "{{cbox_bin}}" ] || { echo "cbox binary not found at {{cbox_bin}} - run 'just build-cbox' or 'just install-cbox' first" >&2; exit 1; }
    cd "{{invocation_directory()}}"
    exec "{{cbox_bin}}" down {{args}}

# List boxes across every per-name home.
# Usage: just list [-a]
list *args:
    #!/usr/bin/env sh
    set -eu
    [ -x "{{cbox_bin}}" ] || { echo "cbox binary not found at {{cbox_bin}} - run 'just build-cbox' or 'just install-cbox' first" >&2; exit 1; }
    cd "{{invocation_directory()}}"
    exec "{{cbox_bin}}" list {{args}}

# Print the installed cbox binary's version (from cbox/Cargo.toml at build
# time), to check what build-cbox or install-cbox left in place.
# Usage: just version
version:
    #!/usr/bin/env sh
    set -eu
    [ -x "{{cbox_bin}}" ] || { echo "cbox binary not found at {{cbox_bin}} - run 'just build-cbox' or 'just install-cbox' first" >&2; exit 1; }
    exec "{{cbox_bin}}" --version
