#!/bin/sh
# End-to-end test for git credentials from stored secrets (docs/design/git-credentials.md).
#
# Not a `cargo test`: it needs a real secret store, a real booted box and a pty, so no CI job runs
# it. Run by hand after building the cbox binary and having an image available:
#
#   just build-cbox   # or: (cd cbox && cargo build --release)
#   ./cbox/tests/git_credentials_e2e.sh [image-name]
#
# `image-name` defaults to `cbox-custom`. Skips itself (exit 0, says why) when a precondition is
# missing: no cbox binary, no such image, no `script(1)`, or not macOS (the secret store is the
# Keychain, and `script -q outfile cmd...` is the BSD form, as in tty_smoke.sh).
#
# What it proves, with a fake token (no network needed):
#   - `secret create --git` needs --user, rejects `_basic` names and a secret that reaches nothing.
#   - A box from a profile with git secrets has the raw placeholder in its env var, an
#     `http.https://<host>/.extraHeader` carrying the `<name>_basic` placeholder for EVERY host of
#     every git secret (including a git-only one with no env var), and a blank credential helper
#     scoped to each host.
#   - The fake token never appears in the box's environment or git config.
#   - A box from a profile with no git secret gets no extraHeader at all.
# With E2E_GITHUB_TOKEN and E2E_PRIVATE_REPO (owner/name) set it also proves a real private
# `git ls-remote` and `gh api user` work from a box that only holds placeholders. The token goes in
# through stdin only, never an argument.
#
# Isolation: scratch BOXLITE_HOME and XDG_CONFIG_HOME (index and profiles) under /tmp. The secrets
# themselves are real Keychain items with unique names; cleanup deletes them.

set -u

image="${1:-cbox-custom}"
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
cbox_bin="$repo_root/cbox/target/release/cbox"
[ -x "$cbox_bin" ] || cbox_bin="$repo_root/cbox/target/debug/cbox"

skip() { echo "git_credentials_e2e: SKIP - $1"; exit 0; }
[ -x "$cbox_bin" ] || skip "no cbox binary at cbox/target/{release,debug}/cbox -- run 'just build-cbox' first"
command -v script >/dev/null 2>&1 || skip "no script(1) on PATH"
docker image inspect "$image" >/dev/null 2>&1 || skip "image '$image' not found locally -- build it first"
[ "$(uname -s)" = Darwin ] || skip "needs the macOS Keychain and BSD script(1); this is $(uname -s)"

tmp="/tmp/cbox-git-e2e-$$" # short: the box's control socket path has a ~104-byte limit
home="$tmp/home"
cfg="$tmp/cfg"
mkdir -p "$home" "$cfg"
fake_token="e2e-fake-token-$$-NEVERLEAK"
sec_gh="e2e-git-$$-gh"   # env var + git, three hosts
sec_gl="e2e-git-$$-gl"   # git only, no env var
sec_pl="e2e-git-$$-pl"   # plain secret, no git
sec_rl="e2e-git-$$-rl"   # real token, optional network check
up_pid=""
failures=0

cb() { BOXLITE_HOME="$home" XDG_CONFIG_HOME="$cfg" "$cbox_bin" "$@"; }

stop_box() {
    [ -n "$up_pid" ] && kill -9 "$up_pid" 2>/dev/null
    up_pid=""
    for pid in $(pgrep -f "boxlite-shim" 2>/dev/null); do
        ps -o command= -p "$pid" 2>/dev/null | grep -qF "$home" && kill -9 "$pid" 2>/dev/null
    done
}

cleanup() {
    stop_box
    for s in "$sec_gh" "$sec_gl" "$sec_pl" "$sec_rl"; do
        cb secret delete "$s" -y >/dev/null 2>&1
    done
    rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

pass() { echo "git_credentials_e2e: PASS - $1"; }
bad() { echo "git_credentials_e2e: FAIL - $1"; failures=$((failures + 1)); }

# expect_fail DESC CMD... : the command must exit non-zero.
expect_fail() {
    desc="$1"; shift
    if "$@" </dev/null >"$tmp/out" 2>&1; then bad "$desc (succeeded)"; else pass "$desc"; fi
}

# expect OUTFILE FIXED-STRING DESC
expect() {
    if tr -d '\r' <"$1" | grep -qF -- "$2"; then pass "$3"; else bad "$3 (missing: $2)"; fi
}
expect_not() {
    if tr -d '\r' <"$1" | grep -qF -- "$2"; then bad "$3 (found: $2)"; else pass "$3"; fi
}

# boot NAME PROFILE: up under a pty, wait for the control socket.
boot() {
    BOXLITE_HOME="$home" XDG_CONFIG_HOME="$cfg" script -q "$tmp/up.out" \
        "$cbox_bin" up "$1" -f --profile "$2" --image "$image" -- sh -c 'sleep 600' >/dev/null 2>&1 &
    up_pid=$!
    waited=0
    while [ ! -S "$home/boxes/$1/cbox.sock" ]; do
        kill -0 "$up_pid" 2>/dev/null || { bad "cbox up exited early for $1 (see $tmp/up.out)"; return 1; }
        waited=$((waited + 2))
        [ "$waited" -lt 120 ] || { bad "control socket for $1 not up after 120s"; return 1; }
        sleep 2
    done
}

# in_box NAME OUTFILE COMMAND: run a shell command in the box under a pty.
in_box() {
    BOXLITE_HOME="$home" XDG_CONFIG_HOME="$cfg" script -q "$2" "$cbox_bin" exec "$1" -- sh -c "$3" >/dev/null 2>&1
}

echo "git_credentials_e2e: creating secrets (scratch config $cfg)"

# --- CLI rules, no box needed ---
expect_fail "--git without --user is rejected" \
    cb secret create "$sec_gh" --host github.com --env GH_TOKEN --git
expect_fail "a secret with neither --env nor --git is rejected" \
    cb secret create "$sec_gh" --host github.com
expect_fail "a name ending in _basic is rejected" \
    cb secret create "x_basic" --host github.com --env X

printf %s "$fake_token" | cb secret create "$sec_gh" --host github.com,api.github.com,gist.github.com \
    --env GH_TOKEN --git --user x-access-token >/dev/null || bad "create $sec_gh"
printf %s "$fake_token" | cb secret create "$sec_gl" --host gitlab.com --git --user oauth2 >/dev/null || bad "create $sec_gl"
printf %s "$fake_token" | cb secret create "$sec_pl" --host api.example.com --env EXAMPLE_KEY >/dev/null || bad "create $sec_pl"

cb secret list >"$tmp/list" 2>&1
grep "^$sec_gh" "$tmp/list" | grep -q "yes" && pass "list marks $sec_gh as git" || bad "list does not mark $sec_gh as git"
grep "^$sec_pl" "$tmp/list" | grep -q "yes" && bad "list marks the plain secret as git" || pass "list does not mark the plain secret as git"

cb profile create e2e-git --secrets "$sec_gh,$sec_gl" >/dev/null || bad "create profile e2e-git"
cb profile create e2e-plain --secrets "$sec_pl" >/dev/null || bad "create profile e2e-plain"

probe='
printf "GH_TOKEN=%s\n" "$GH_TOKEN"
for h in github.com api.github.com gist.github.com gitlab.com; do
  printf "HDR %s=%s\n" "$h" "$(git config --global --get "http.https://$h/.extraHeader")"
  printf "HELPER %s=[%s]\n" "$h" "$(git config --global --get "credential.https://$h.helper")"
done
printf "EXTRA_COUNT=%s\n" "$(git config --global --get-regexp "extraheader" | wc -l | tr -d " ")"
(env; cat ~/.gitconfig /etc/gitconfig 2>/dev/null) | grep -c NEVERLEAK | sed "s/^/LEAKS=/"
echo PROBE_DONE
'

# --- Box with git secrets ---
echo "git_credentials_e2e: booting box with profile e2e-git"
if boot e2e-git-box e2e-git; then
    in_box e2e-git-box "$tmp/git.out" "$probe"
    expect "$tmp/git.out" "PROBE_DONE" "probe ran in the box"
    expect "$tmp/git.out" "GH_TOKEN=<BOXLITE_SECRET:$sec_gh>" "GH_TOKEN carries the raw placeholder"
    for h in github.com api.github.com gist.github.com; do
        expect "$tmp/git.out" "HDR $h=Authorization: Basic <BOXLITE_SECRET:${sec_gh}_basic>" "extraHeader for $h"
        expect "$tmp/git.out" "HELPER $h=[]" "blank credential helper for $h"
    done
    expect "$tmp/git.out" "HDR gitlab.com=Authorization: Basic <BOXLITE_SECRET:${sec_gl}_basic>" "git-only secret writes its extraHeader"
    expect "$tmp/git.out" "HELPER gitlab.com=[]" "blank credential helper for gitlab.com"
    expect "$tmp/git.out" "LEAKS=0" "the token is in neither the environment nor git config"
else
    bad "box e2e-git-box did not boot"
fi
stop_box

# --- Box without git secrets: nothing may be written ---
echo "git_credentials_e2e: booting box with profile e2e-plain"
if boot e2e-plain-box e2e-plain; then
    in_box e2e-plain-box "$tmp/plain.out" "$probe"
    expect "$tmp/plain.out" "PROBE_DONE" "probe ran in the plain box"
    expect "$tmp/plain.out" "EXTRA_COUNT=0" "no extraHeader without a git secret"
    expect "$tmp/plain.out" "HDR github.com=" "github.com extraHeader is empty"
    expect_not "$tmp/plain.out" "BOXLITE_SECRET:${sec_gh}_basic" "no placeholder from the other profile"
else
    bad "box e2e-plain-box did not boot"
fi
stop_box

# --- Optional: a real token against a real private repo ---
if [ -n "${E2E_GITHUB_TOKEN:-}" ] && [ -n "${E2E_PRIVATE_REPO:-}" ]; then
    echo "git_credentials_e2e: real-token check against $E2E_PRIVATE_REPO"
    printf %s "$E2E_GITHUB_TOKEN" | cb secret create "$sec_rl" --host github.com,api.github.com,gist.github.com \
        --env GH_TOKEN --git --user x-access-token >/dev/null || bad "create $sec_rl"
    cb profile create e2e-real --secrets "$sec_rl" >/dev/null || bad "create profile e2e-real"
    if boot e2e-real-box e2e-real; then
        in_box e2e-real-box "$tmp/real.out" "git ls-remote https://github.com/$E2E_PRIVATE_REPO.git HEAD >/dev/null 2>&1 && echo LSREMOTE_OK; gh api user --jq .login >/dev/null 2>&1 && echo GH_OK; echo REAL_DONE"
        expect "$tmp/real.out" "LSREMOTE_OK" "git ls-remote of the private repo works through the proxy"
        expect "$tmp/real.out" "GH_OK" "gh api user works through the proxy"
    else
        bad "box e2e-real-box did not boot"
    fi
    stop_box
else
    echo "git_credentials_e2e: SKIP real-token check (set E2E_GITHUB_TOKEN and E2E_PRIVATE_REPO to run it)"
fi

if [ "$failures" -eq 0 ]; then
    echo "git_credentials_e2e: ALL PASS"
    exit 0
fi
echo "git_credentials_e2e: $failures check(s) failed"
exit 1
