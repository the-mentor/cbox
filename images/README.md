# images/ (draft)

Draft of an agent-free base image plus one thin image per coding agent. **CI does
not build this directory yet**, and nothing here replaces `base/` or `custom/`,
which are still what `just build` and `cbox up` use. Design and open questions:
`docs/design/agent-images.md`.

## Layout

- `Dockerfile`: one multi-stage file. Stages:
  - `base`: OS, shell, git, Node, `gh`, `uv`, `pre-commit`, oh-my-posh, the
    `sandbox-user` user. No agent.
  - `claude`: `base` + Claude Code, its wrapper and the `cc` link, and default
    user-scope settings from `claude/`.
  - `codex`: `base` + the Codex CLI.
  - `opencode`: `base` + opencode, with its global config from `opencode/`.
- `docker-bake.hcl`: builds `base` and each agent as `cbox-agent-<name>:dev`.
- `base/`, `claude/`, `opencode/`: files the stages copy in.

## Build locally

From the repo root:

```bash
docker buildx bake -f images/docker-bake.hcl agents          # claude, codex, opencode
docker buildx bake -f images/docker-bake.hcl agent-claude    # just one
docker buildx build --target claude -f images/Dockerfile images/
```

Override defaults with a separate image:

```dockerfile
ARG BASE_IMAGE=cbox-agent-claude:dev
FROM ${BASE_IMAGE}
COPY --chown=sandbox-user:sandbox-user settings.json /home/sandbox-user/.claude/settings.json
```
