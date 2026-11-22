#!/bin/sh
# Smoke test for the images built from images/Dockerfile. Each check runs a
# short command inside the image and the script exits 1 if any fails.
#
#   docker buildx bake -f images/docker-bake.hcl --load   # build the :dev tags
#   sh images/smoke-test.sh [name-prefix] [tag]            # default: cbox-agent dev
#
# It needs docker and nothing else. It does not boot a box, so it cannot check
# the gateway connection, first-run prompts, iTerm2 or a model call.

# shellcheck disable=SC2016 # the check scripts are single-quoted on purpose: they expand inside the image
prefix=${1:-cbox-agent}
tag=${2:-dev}
here=$(cd "$(dirname "$0")" && pwd)
fail=0

# The version an ARG in the Dockerfile pins, e.g. pin CODEX_VERSION.
pin() { sed -n "s/^ARG $1=//p" "$here/Dockerfile"; }

# check <name> <image> <sh script>: run the script in the image.
check() {
    name=$1
    image=$prefix-$2:$tag
    if out=$(docker run --rm "$image" sh -c "$3" 2>&1); then
        echo "ok   $name"
    else
        echo "FAIL $name"
        echo "$out" | sed 's/^/       /'
        fail=1
    fi
}

check "base: user and directory" base '[ "$(id -un)" = sandbox-user ] && [ "$PWD" = /workspace ]'
check "base: tools run" base 'jq --version && yq --version && gh --version && uv --version && pre-commit --version && oh-my-posh version && ps --version && git --version'
check "base: no agent installed" base '! command -v claude && ! command -v codex && ! command -v opencode && ! command -v sandbox-agent'

check "claude: sandbox-agent starts claude (version matches the pin)" claude "sandbox-agent --version | grep -F '$(pin CLAUDE_CODE_VERSION)'"
check "claude: version matches the pin" claude "claude --version | grep -F '$(pin CLAUDE_CODE_VERSION)'"
check "claude: wrapper, cc and hook are installed" claude 'command -v claude-real && command -v cc && command -v cbox-hook'
check "claude: config files parse" claude 'jq -e . ~/.claude.json ~/.claude/settings.json >/dev/null'
check "claude: wrapper approves ANTHROPIC_API_KEY" claude 'ANTHROPIC_API_KEY=abcdefghijklmnopqrstuvwxyz claude --version >/dev/null && jq -e ".customApiKeyResponses.approved | length == 1" ~/.claude.json >/dev/null'
check "claude: gateway MCP entry and theme file" claude 'jq -e ".mcpServers.agentgateway.url" ~/.claude.json >/dev/null && [ -f ~/mojada-claude.omp.json ]'
check "claude: cbox-hook prints a terminalSequence" claude 'echo "{\"hook_event_name\":\"Stop\"}" | TERM_PROGRAM=iTerm.app cbox-hook | grep -q terminalSequence'

check "codex: sandbox-agent starts codex (version matches the pin)" codex "sandbox-agent --version | grep -F '$(pin CODEX_VERSION)'"
check "codex: version matches the pin" codex "codex --version | grep -F '$(pin CODEX_VERSION)'"
check "codex: config parses and has the gateway MCP entry" codex 'python3 -c "import tomllib,os; c=tomllib.load(open(os.path.expanduser(\"~/.codex/config.toml\"),\"rb\")); assert c[\"mcp_servers\"][\"agentgateway\"][\"url\"]"'

check "opencode: sandbox-agent starts opencode (version matches the pin)" opencode "sandbox-agent --version | grep -F '$(pin OPENCODE_VERSION)'"
check "opencode: version matches the pin" opencode "opencode --version | grep -F '$(pin OPENCODE_VERSION)'"
check "opencode: config parses and has the gateway MCP entry" opencode 'jq -e ".mcp.servers.agentgateway.url" ~/.config/opencode/opencode.json >/dev/null'

[ "$fail" = 0 ] && echo "all checks passed" || echo "some checks failed"
exit "$fail"
