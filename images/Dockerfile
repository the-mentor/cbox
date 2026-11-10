# syntax=docker/dockerfile:1
# DRAFT, not built by CI. One file, one stage per image; pick with --target
# (base, claude, codex, opencode) or build them all with docker-bake.hcl.
# See docs/design/agent-images.md. base/ and custom/ are the images in use.

# The OS + Node image the agent-free base starts from. A customize image is a
# separate Dockerfile with its own `ARG BASE_IMAGE=cbox-agent-claude:dev`.
# Pin a tag or @sha256 digest here once the design is final.
ARG BASE_IMAGE=node:26-trixie-slim

# ---------------------------------------------------------------------------
# base: OS, shell, git, runtimes, the non-root user, common tooling. NO agent.
# Same as base/Dockerfile minus Claude Code, its wrapper and the `cc` link.
# ---------------------------------------------------------------------------
FROM ${BASE_IMAGE} AS base
RUN apt update \
    && apt upgrade -y \
    && apt install -y --no-install-recommends sudo git curl wget ca-certificates python3 python3-pip just unzip jq bash-completion lazygit \
    && rm -rf /var/lib/apt/lists/*

# Non-login shells skip /etc/profile.d, so source our own file from bash.bashrc.
COPY base/bashrc.d/boxlite.sh /etc/bashrc.d/boxlite.sh
RUN echo '[ -f /etc/bashrc.d/boxlite.sh ] && . /etc/bashrc.d/boxlite.sh' >> /etc/bash.bashrc

RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
    && apt update \
    && apt install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/*

# Shared themes path: root's cache is unreadable by sandbox-user.
ENV POSH_THEMES_PATH=/usr/local/share/oh-my-posh/themes
RUN curl -s https://ohmyposh.dev/install.sh | bash -s -- -d /usr/local/bin -t "$POSH_THEMES_PATH" \
    && rm -rf /root/.cache/oh-my-posh

ARG UV_VERSION=0.13.7
RUN pip install --no-cache-dir --break-system-packages "uv==${UV_VERSION}"

# uv tool venv keeps pre-commit's deps away from Debian's; shared dirs for sandbox-user.
ARG PRE_COMMIT_VERSION=4.7.0
RUN UV_TOOL_DIR=/opt/uv-tools UV_TOOL_BIN_DIR=/usr/local/bin \
    uv tool install --no-cache "pre-commit==${PRE_COMMIT_VERSION}"

# Rename the node image's uid-1000 user; passwordless sudo for apt and the like.
RUN usermod -l sandbox-user -d /home/sandbox-user -m node \
    && groupmod -n sandbox-user node \
    && echo 'sandbox-user ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/sandbox-user \
    && chmod 0440 /etc/sudoers.d/sandbox-user \
    && install -d -o sandbox-user -g sandbox-user /workspace

USER sandbox-user
WORKDIR /workspace

# ---------------------------------------------------------------------------
# claude: base + Claude Code + its generic defaults (cbox/gateway bits are
# left to a customize image, see the design doc).
# ---------------------------------------------------------------------------
FROM base AS claude
USER root
# https://code.claude.com/docs/en/setup#install-with-npm
ARG CLAUDE_CODE_VERSION=2.1.322
RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
    && npm cache clean --force \
    && rm -rf /tmp/node-compile-cache

# Pre-approves ANTHROPIC_API_KEY in ~/.claude.json, so fresh boxes skip the prompt.
COPY --chmod=0755 claude/claude-wrapper.sh /usr/local/bin/claude-wrapper
RUN mv /usr/local/bin/claude /usr/local/bin/claude-real \
    && ln -sf /usr/local/bin/claude-wrapper /usr/local/bin/claude \
    && ln -sf /usr/local/bin/claude-wrapper /usr/local/bin/cc

USER sandbox-user
# User scope: the lowest-precedence settings, so anything above can override them.
COPY --chown=sandbox-user:sandbox-user claude/claude.json /home/sandbox-user/.claude.json
COPY --chown=sandbox-user:sandbox-user claude/settings.json /home/sandbox-user/.claude/settings.json

# ---------------------------------------------------------------------------
# codex: base + OpenAI Codex CLI. No default config yet (open question).
# ---------------------------------------------------------------------------
FROM base AS codex
USER root
# https://github.com/openai/codex#quickstart (npm install -g @openai/codex)
ARG CODEX_VERSION=0.167.0
RUN npm install -g "@openai/codex@${CODEX_VERSION}" \
    && npm cache clean --force \
    && rm -rf /tmp/node-compile-cache
USER sandbox-user

# ---------------------------------------------------------------------------
# opencode: base + opencode CLI + global config (update checks off).
# ---------------------------------------------------------------------------
FROM base AS opencode
USER root
# https://opencode.ai/docs/ (npm install -g @opencode/cli)
ARG OPENCODE_VERSION=2.2.6
RUN npm install -g "@opencode/cli@${OPENCODE_VERSION}" \
    && npm cache clean --force \
    && rm -rf /tmp/node-compile-cache
USER sandbox-user
# Global config, the lowest-precedence opencode file (https://opencode.ai/docs/config).
COPY --chown=sandbox-user:sandbox-user opencode/opencode.json /home/sandbox-user/.config/opencode/opencode.json
