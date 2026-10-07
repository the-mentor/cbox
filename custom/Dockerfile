# Published multi-arch base (see docs/design/images.md). `just build-local`
# overrides this with a locally built cbox-base.
ARG BASE_IMAGE=ghcr.io/the-mentor/cbox-base:latest
FROM ${BASE_IMAGE}

# The base image ends as USER sandbox-user; package installs need root.
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends awscli \
    && rm -rf /var/lib/apt/lists/*
# Forwards Claude Code hook events to cbox on the host (iTerm2 integration),
# wired up as hooks in settings.json below.
COPY --chmod=0755 cbox-hook.sh /usr/local/bin/cbox-hook
USER sandbox-user

# Mark first-run onboarding complete so the interactive TUI uses
# CLAUDE_CODE_OAUTH_TOKEN instead of prompting to pick a login method.
COPY --chown=sandbox-user:sandbox-user claude.json /home/sandbox-user/.claude.json

# statusLine is read from ~/.claude/settings.json, NOT ~/.claude.json — putting it
# in claude.json is silently ignored. Copied before the plugin install below, which
# merges enabledPlugins/extraKnownMarketplaces into this same file.
COPY --chown=sandbox-user:sandbox-user settings.json /home/sandbox-user/.claude/settings.json

# Marketplaces (comma-separated GitHub repos) and plugins (comma-separated
# plugin@marketplace ids) to bake in. Append to add more, or override at build:
#   docker build --build-arg PLUGINS="a@m,b@m" custom/
ARG MARKETPLACES="anthropics/claude-plugins-official,dietrichGebert/ponytail,the-mentor/no-ai-attribution"
ARG PLUGINS="superpowers@claude-plugins-official,ponytail@ponytail,no-ai-attribution@no-ai-attribution"
RUN set -eu; IFS=,; \
    for m in $MARKETPLACES; do claude plugin marketplace add "$m"; done; \
    for p in $PLUGINS; do claude plugin install --scope user "$p"; done

# Private marketplaces/plugins, same format as above. Set via CBOX_PRIVATE_* in
# .env, never here: this repo is public. `just build-image` passes a GitHub token
# as a BuildKit secret, mounted for this RUN only. git reads it through a
# credential helper set in env vars scoped to this RUN, so it never lands in
# a layer, ~/.gitconfig, or the clone's remote URL. No-op when unset.
ARG PRIVATE_MARKETPLACES=""
ARG PRIVATE_PLUGINS=""
RUN --mount=type=secret,id=gh_token,uid=1000 set -eu; \
    [ -n "$PRIVATE_MARKETPLACES" ] || exit 0; \
    [ -s /run/secrets/gh_token ] || { echo "PRIVATE_MARKETPLACES set but no gh_token secret" >&2; exit 1; }; \
    export GIT_TERMINAL_PROMPT=0 GIT_CONFIG_COUNT=1 \
        GIT_CONFIG_KEY_0=credential.https://github.com.helper \
        GIT_CONFIG_VALUE_0='!f() { test "$1" = get && echo username=x-access-token && echo "password=$(cat /run/secrets/gh_token)"; }; f'; \
    IFS=,; \
    for m in $PRIVATE_MARKETPLACES; do claude plugin marketplace add "$m"; done; \
    for p in $PRIVATE_PLUGINS; do claude plugin install --scope user "$p"; done

# copy oh-my-posh statusline config
COPY --chown=sandbox-user:sandbox-user mojada-claude.omp.json /home/sandbox-user/mojada-claude.omp.json

WORKDIR /workspace
