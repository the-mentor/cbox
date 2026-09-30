FROM node:26-trixie-slim
RUN apt update \
    && apt upgrade -y \
    && apt install -y --no-install-recommends sudo git curl wget ca-certificates python3 python3-pip just unzip jq bash-completion lazygit \
    && rm -rf /var/lib/apt/lists/*

# /etc/profile.d hooks (incl. bash-completion's) only run for login shells;
# boxlite's interactive session is a non-login shell, so source our own
# customizations file from /etc/bash.bashrc too. Add future bashrc tweaks to
# bashrc.d/boxlite.sh rather than growing this RUN step.
COPY bashrc.d/boxlite.sh /etc/bashrc.d/boxlite.sh
RUN echo '[ -f /etc/bashrc.d/boxlite.sh ] && . /etc/bashrc.d/boxlite.sh' >> /etc/bash.bashrc

RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
    && apt update \
    && apt install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/*

# npm keeps a ~100MB copy of the downloaded package in its cache, and node
# leaves a compile cache in /tmp; neither is used at runtime.
RUN npm install -g @anthropic-ai/claude-code \
    && npm cache clean --force \
    && rm -rf /tmp/node-compile-cache

# Themes go to a shared path: the default (root's cache) is unreadable by
# sandbox-user. Use one with --config $POSH_THEMES_PATH/<theme>.omp.json.
ENV POSH_THEMES_PATH=/usr/local/share/oh-my-posh/themes
RUN curl -s https://ohmyposh.dev/install.sh | bash -s -- -d /usr/local/bin -t "$POSH_THEMES_PATH" \
    && rm -rf /root/.cache/oh-my-posh

# Wrap the claude binary to pre-approve ANTHROPIC_API_KEY in ~/.claude.json at
# startup, silencing the "Detected a custom API key" interactive prompt in
# ephemeral boxes where the config file starts fresh on every boot.
COPY --chmod=0755 claude-wrapper.sh /usr/local/bin/claude-wrapper
RUN mv /usr/local/bin/claude /usr/local/bin/claude-real \
    && ln -sf /usr/local/bin/claude-wrapper /usr/local/bin/claude \
    && ln -sf /usr/local/bin/claude-wrapper /usr/local/bin/cc

ARG UV_VERSION=0.12.21
ARG PRE_COMMIT_VERSION=4.6.2
RUN pip install --no-cache-dir --break-system-packages "uv==${UV_VERSION}" "pre-commit==${PRE_COMMIT_VERSION}"

# Run as a generic non-root user, with passwordless sudo for apt installs and
# the like. The node image's own uid-1000 user is renamed rather than adding a
# second one. BoxLite honours USER for both the init process and exec sessions,
# and derives HOME (/home/sandbox-user) from /etc/passwd.
RUN usermod -l sandbox-user -d /home/sandbox-user -m node \
    && groupmod -n sandbox-user node \
    && echo 'sandbox-user ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/sandbox-user \
    && chmod 0440 /etc/sudoers.d/sandbox-user \
    && install -d -o sandbox-user -g sandbox-user /workspace

USER sandbox-user
WORKDIR /workspace
