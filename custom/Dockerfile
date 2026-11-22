# The published claude agent image (see docs/design/agent-images.md). It already has
# the claude CLI, ~/.claude.json, ~/.claude/settings.json (hooks, status line, MCP
# entry), cbox-hook and the status line theme; this layer adds awscli and plugins.
# CBOX_BASE_IMAGE=ghcr.io/the-mentor/cbox-base:latest builds on the old base instead, which
# has none of those defaults.
ARG BASE_IMAGE=ghcr.io/the-mentor/cbox-agent-claude:latest
FROM ${BASE_IMAGE}

# The base image ends as USER sandbox-user; package installs need root.
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends awscli \
    && rm -rf /var/lib/apt/lists/*
USER sandbox-user

# The plugin installs below merge enabledPlugins/extraKnownMarketplaces into the
# image's ~/.claude/settings.json.

# Marketplaces (comma-separated GitHub repos) and plugins (comma-separated
# plugin@marketplace ids) to bake in. Append to add more, or override at build:
#   docker build --build-arg CBOX_PLUGINS="a@m,b@m" custom/
ARG CBOX_MARKETPLACES="anthropics/claude-plugins-official,dietrichGebert/ponytail,the-mentor/no-ai-attribution"
ARG CBOX_PLUGINS="superpowers@claude-plugins-official,pr-review-toolkit@claude-plugins-official,ponytail@ponytail,no-ai-attribution@no-ai-attribution"
RUN set -eu; IFS=,; \
    for m in $CBOX_MARKETPLACES; do claude plugin marketplace add "$m"; done; \
    for p in $CBOX_PLUGINS; do claude plugin install --scope user "$p"; done

# Private marketplaces/plugins, same format as above. Set via CBOX_PRIVATE_* in
# .env, never here: this repo is public. `just build-image` passes a GitHub token
# as a BuildKit secret, mounted for this RUN only. git reads it through a
# credential helper set in env vars scoped to this RUN, so it never lands in
# a layer, ~/.gitconfig, or the clone's remote URL. No-op when unset.
ARG CBOX_PRIVATE_MARKETPLACES=""
ARG CBOX_PRIVATE_PLUGINS=""
RUN --mount=type=secret,id=gh_token,uid=1000 set -eu; \
    [ -n "$CBOX_PRIVATE_MARKETPLACES" ] || exit 0; \
    [ -s /run/secrets/gh_token ] || { echo "CBOX_PRIVATE_MARKETPLACES set but no gh_token secret" >&2; exit 1; }; \
    export GIT_TERMINAL_PROMPT=0 GIT_CONFIG_COUNT=1 \
        GIT_CONFIG_KEY_0=credential.https://github.com.helper \
        GIT_CONFIG_VALUE_0='!f() { test "$1" = get && echo username=x-access-token && echo "password=$(cat /run/secrets/gh_token)"; }; f'; \
    IFS=,; \
    for m in $CBOX_PRIVATE_MARKETPLACES; do claude plugin marketplace add "$m"; done; \
    for p in $CBOX_PRIVATE_PLUGINS; do claude plugin install --scope user "$p"; done

WORKDIR /workspace
