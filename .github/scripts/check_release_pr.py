#!/usr/bin/env python3
"""Refuse to auto-merge a release PR unless it only bumps the repo's version
(cbox's Cargo.toml/Cargo.lock) and prepends to CHANGELOG.md.

Usage: check_release_pr.py BASE_REF HEAD_SHA
Compares git objects directly, so the merge must be pinned to HEAD_SHA.
"""
import json
import subprocess
import sys
import tomllib

CARGO_TOML = "cbox/Cargo.toml"
CARGO_LOCK = "cbox/Cargo.lock"
MANIFEST = ".release-please-manifest.json"
CHANGELOG = "CHANGELOG.md"
ALLOWED = {CARGO_TOML, CARGO_LOCK, MANIFEST, CHANGELOG}


def git(*args):
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout


def show(ref, path):
    try:
        return git("show", f"{ref}:{path}")
    except subprocess.CalledProcessError:
        return None


def without_cbox_version(path, text):
    data = tomllib.loads(text)
    if path == CARGO_TOML:
        data["package"].pop("version", None)
    else:
        for pkg in data.get("package", []):
            if pkg.get("name") == "cbox":
                pkg.pop("version", None)
    return data


def check(base, head):
    errors = []
    changed = set(git("diff", "--name-only", f"{base}...{head}").split())
    for path in sorted(changed - ALLOWED):
        errors.append(f"unexpected file changed: {path}")

    for path in (CARGO_TOML, CARGO_LOCK):
        old, new = show(base, path), show(head, path)
        if old is None or new is None:
            errors.append(f"{path}: missing on base or head")
        elif without_cbox_version(path, old) != without_cbox_version(path, new):
            errors.append(f"{path}: changes something other than cbox's version")

    manifest = show(head, MANIFEST)
    if manifest is None or list(json.loads(manifest)) != ["."]:
        errors.append(f"{MANIFEST}: must contain only the '.' package")

    old_log, new_log = show(base, CHANGELOG) or "", show(head, CHANGELOG)
    if new_log is None:
        errors.append(f"{CHANGELOG}: missing on head")
    else:
        # release-please inserts the new entry under the title line.
        title, _, rest = old_log.partition("\n")
        # The first release creates the file, so there is nothing to preserve.
        if old_log and not (new_log.startswith(title + "\n") and new_log.endswith(rest)):
            errors.append(f"{CHANGELOG}: existing entries were modified")

    return errors


if __name__ == "__main__":
    problems = check(sys.argv[1], sys.argv[2])
    for p in problems:
        print(f"::error::{p}")
    sys.exit(1 if problems else 0)
