# images/ (draft)

Draft of an agent-free base image plus one thin image per coding agent. CI builds and
publishes these as `ghcr.io/the-mentor/cbox-agent-<name>`. `custom/` builds on
`cbox-agent-claude`, and `cbox up` / `cbox exec` run this directory's `sandbox-agent` by
default; `cbox up` still boots `cbox-custom` by default, and `base/` and `base-rust/` are
unchanged. Design and open questions: `docs/design/agent-images.md`.

## Layout

- `Dockerfile`: one multi-stage file. Stages:
  - `base`: OS, shell, git, Node, `gh`, `jq`, `yq`, `uv`, `pre-commit`, oh-my-posh, the
    `sandbox-user` user. No agent.
  - `claude`: `base` + Claude Code, its wrapper and the `cc` link, and default
    user-scope settings from `claude/`.
  - `codex`: `base` + the Codex CLI, with its user config from `codex/`.
  - `opencode`: `base` + opencode, with its global config from `opencode/`.
- `docker-bake.hcl`: builds `base` and each agent as `cbox-agent-<name>:dev`.
- `base/`, `claude/`, `codex/`, `opencode/`: files the stages copy in.

## Build locally

From the repo root:

```bash
docker buildx bake -f images/docker-bake.hcl agents          # claude, codex, opencode
docker buildx bake -f images/docker-bake.hcl agent-claude    # just one
docker buildx build --load -t cbox-agent-claude:dev --target claude -f images/Dockerfile images/
```

Override defaults with a separate image. Merge a small `overrides.json` into the default
settings so you only state what changes (an array you set, such as `hooks.Stop` or
`permissions.allow`, replaces the default one rather than adding to it):

```dockerfile
ARG BASE_IMAGE=cbox-agent-claude:dev
FROM ${BASE_IMAGE}
COPY overrides.json /tmp/overrides.json
RUN f=$HOME/.claude/settings.json \
    && jq -s '.[0] * .[1]' "$f" /tmp/overrides.json > /tmp/merged.json \
    && mv /tmp/merged.json "$f" && rm /tmp/overrides.json
```

## Boot a box from one

`just build-agents` builds the images and pushes the agents to the local registry (`localhost:5551`,
started for you), then boot one by name. `cbox` runs the image's `sandbox-agent` by default:

```bash
just build-agents
just up -i cbox-agent-codex -f
just up -i cbox-agent-opencode -f
```

`sandbox-agent` is a small launcher in the base image: it starts whichever agent the image has
(set `SANDBOX_AGENT` if there are several), and `--continue` resumes its last session.
`cbox up` runs it by default and `cbox exec` runs `sandbox-agent --continue`.

The images CI publishes can be used directly too: `just up -i ghcr.io/the-mentor/cbox-agent-codex:latest`.

## Smoke test

After building the `:dev` tags, `sh images/smoke-test.sh` checks the user, tools, pinned
versions and baked config in each image. CI runs the same script.
