# syntax=docker/dockerfile:1
# Built and published by CI (.github/workflows/agent-images.yml). One file, one stage
# per image; pick with --target (base, claude, codex, opencode) or build them all
# with docker-bake.hcl. custom/ builds on the claude stage. See
# docs/design/agent-images.md.

# The OS + Node image the agent-free base starts from. A customize image is a
# separate Dockerfile with its own `ARG BASE_IMAGE=cbox-agent-claude:dev`.
# Pin a tag or @sha256 digest here once the design is final.
ARG BASE_IMAGE=node:26-trixie-slim

# ---------------------------------------------------------------------------
# base: OS, shell, git, runtimes, the non-root user, common tooling. NO agent.
# Same as base/Dockerfile minus Claude Code, its wrapper and the `cc` link, plus yq,
# procps (Codex needs `ps`) and the sandbox-agent launcher.
# ---------------------------------------------------------------------------
FROM ${BASE_IMAGE} AS base
RUN apt update \
    && apt upgrade -y \
    && apt install -y --no-install-recommends sudo git curl wget ca-certificates python3 python3-pip just unzip jq yq procps bash-completion lazygit \
    && rm -rf /var/lib/apt/lists/*

# Non-login shells skip /etc/profile.d, so source our own file from bash.bashrc.
COPY base/bashrc.d/boxlite.sh /etc/bashrc.d/boxlite.sh
RUN echo '[ -f /etc/bashrc.d/boxlite.sh ] && . /etc/bashrc.d/boxlite.sh' >> /etc/bash.bashrc

# What `cbox up` runs by default: starts whichever agent the image has (see the design doc).
COPY --chmod=0755 base/sandbox-agent /usr/local/bin/sandbox-agent

RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
    && apt update \
    && apt install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/*

# Shared themes path: root's cache is unreadable by sandbox-user.
ENV POSH_THEMES_PATH=/usr/local/share/oh-my-posh/themes
# Downloaded to a file first: `curl | bash` hides a failed download (no pipefail in RUN).
# -v pins the binary (the tag keeps its leading v). The script always fetches the themes
# from `latest`, whatever -v says; only an interactive `oh-my-posh init` uses them, the
# Claude status line uses its own theme file.
# renovate: datasource=github-releases depName=JanDeDobbeleer/oh-my-posh
ARG OH_MY_POSH_VERSION=v33.2.0
RUN curl -fsSL https://ohmyposh.dev/install.sh -o /tmp/omp-install.sh \
    && bash /tmp/omp-install.sh -v "$OH_MY_POSH_VERSION" -d /usr/local/bin -t "$POSH_THEMES_PATH" \
    && rm -f /tmp/omp-install.sh \
    && rm -rf /root/.cache/oh-my-posh

# renovate: datasource=pypi depName=uv
ARG UV_VERSION=0.13.9
RUN pip install --no-cache-dir --break-system-packages "uv==${UV_VERSION}"

# uv tool venv keeps pre-commit's deps away from Debian's; shared dirs for sandbox-user.
# renovate: datasource=pypi depName=pre-commit
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
# claude: base + Claude Code + its default user-scope config, including the cbox
# iTerm2 hook, the status line and the agentgateway MCP entry (see the design doc).
# ---------------------------------------------------------------------------
FROM base AS claude
USER root
# https://code.claude.com/docs/en/setup#install-with-npm
# renovate: datasource=npm depName=@anthropic-ai/claude-code
ARG CLAUDE_CODE_VERSION=2.1.330
RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
    && npm cache clean --force \
    && rm -rf /tmp/node-compile-cache

# Pre-approves ANTHROPIC_API_KEY in ~/.claude.json, so fresh boxes skip the prompt.
COPY --chmod=0755 claude/claude-wrapper.sh /usr/local/bin/claude-wrapper
RUN mv /usr/local/bin/claude /usr/local/bin/claude-real \
    && ln -sf /usr/local/bin/claude-wrapper /usr/local/bin/claude \
    && ln -sf /usr/local/bin/claude-wrapper /usr/local/bin/cc

# Forwards Claude Code hook events to cbox on the host (iTerm2 integration),
# wired up as hooks in settings.json below.
COPY --chmod=0755 claude/cbox-hook.sh /usr/local/bin/cbox-hook

USER sandbox-user
# User scope: the lowest-precedence settings, so anything above can override them.
COPY --chown=sandbox-user:sandbox-user claude/claude.json /home/sandbox-user/.claude.json
COPY --chown=sandbox-user:sandbox-user claude/settings.json /home/sandbox-user/.claude/settings.json
# The status line in settings.json runs oh-my-posh (in base) with this theme.
COPY --chown=sandbox-user:sandbox-user claude/mojada-claude.omp.json /home/sandbox-user/mojada-claude.omp.json

# ---------------------------------------------------------------------------
# codex: base + OpenAI Codex CLI + user-scope config (update check off, /workspace trusted).
# ---------------------------------------------------------------------------
FROM base AS codex
USER root
# https://github.com/openai/codex#quickstart (npm install -g @openai/codex)
# renovate: datasource=npm depName=@openai/codex
ARG CODEX_VERSION=0.171.0
RUN npm install -g "@openai/codex@${CODEX_VERSION}" \
    && npm cache clean --force \
    && rm -rf /tmp/node-compile-cache
USER sandbox-user
# User scope: Codex's user file outranks the system one, so a customize image replaces it.
COPY --chown=sandbox-user:sandbox-user codex/config.toml /home/sandbox-user/.codex/config.toml

# ---------------------------------------------------------------------------
# opencode: base + opencode CLI + global config (update checks off).
# ---------------------------------------------------------------------------
FROM base AS opencode
USER root
# https://opencode.ai/docs/ (npm install -g @opencode/cli)
# renovate: datasource=npm depName=@opencode/cli
ARG OPENCODE_VERSION=2.3.2
RUN npm install -g "@opencode/cli@${OPENCODE_VERSION}" \
    && npm cache clean --force \
    && rm -rf /tmp/node-compile-cache
USER sandbox-user
# Global config, the lowest-precedence opencode file (https://opencode.ai/docs/config).
COPY --chown=sandbox-user:sandbox-user opencode/opencode.json /home/sandbox-user/.config/opencode/opencode.json
