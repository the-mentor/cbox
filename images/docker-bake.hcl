# DRAFT: local builds only. Nothing publishes these images yet (no registry,
# no CI job); tags are local names and `output = type=docker` loads each image into
# the local docker, whichever builder is active (docker or docker-container driver).
# Run from the repo root: docker buildx bake -f images/docker-bake.hcl [agents]

group "default" {
  targets = ["base", "agents"]
}

group "agents" {
  targets = ["agent-claude", "agent-codex", "agent-opencode"]
}

target "base" {
  context    = "images"
  dockerfile = "Dockerfile"
  target     = "base"
  tags       = ["cbox-agent-base:dev"]
  output     = ["type=docker"]
}

target "agent" {
  name = "agent-${a}"
  matrix = {
    a = ["claude", "codex", "opencode"]
  }
  context    = "images"
  dockerfile = "Dockerfile"
  target     = a
  tags       = ["cbox-agent-${a}:dev"]
  output     = ["type=docker"]
}
