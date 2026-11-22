#!/bin/sh
# Tests images/base/sandbox-agent against stub agents. Needs no docker: each case
# puts stub `claude`/`codex`/`opencode` scripts (that print how they were called)
# on a private PATH. The smoke test only runs the launcher in single-agent images,
# so this is what covers the ambiguity error, SANDBOX_AGENT and the --continue
# mapping.
#
#   sh images/test-sandbox-agent.sh

here=$(cd "$(dirname "$0")" && pwd)
launcher=$here/base/sandbox-agent
fail=0

# run <agents-installed> <env assignments...> -- <launcher args...>
# prints "<exit code>|<stdout and stderr>"
run() {
    agents=$1
    shift
    dir=$(mktemp -d)
    for a in $agents; do
        printf '#!/bin/sh\necho "%s $*"\n' "$a" > "$dir/$a"
        chmod +x "$dir/$a"
    done
    envs=""
    while [ "$1" != -- ]; do envs="$envs $1"; shift; done
    shift
    envs=$(printf '%s' "$envs" | sed "s|@DIR@|$dir|g")  # @DIR@ is this case's stub directory
    # shellcheck disable=SC2086 # $envs is a list of NAME=value words
    out=$(env -i PATH="$dir:/usr/bin:/bin" $envs sh "$launcher" "$@" 2>&1)
    rc=$?
    rm -rf "$dir"
    echo "$rc|$out"
}

# expect <name> <expected "rc|output"> <actual>
expect() {
    if [ "$2" = "$3" ]; then
        echo "ok   $1"
    else
        echo "FAIL $1"
        echo "       want: $2"
        echo "       got:  $3"
        fail=1
    fi
}

# contains <name> <exit code> <text> <actual>
contains() {
    case "$4" in
        "$2|"*"$3"*) echo "ok   $1" ;;
        *) echo "FAIL $1"; echo "       want exit $2 and text: $3"; echo "       got: $4"; fail=1 ;;
    esac
}

expect "claude alone starts claude" "0|claude " "$(run claude --)"
expect "codex alone starts codex" "0|codex " "$(run codex --)"
expect "opencode alone starts opencode" "0|opencode " "$(run opencode --)"

expect "claude --continue" "0|claude --continue" "$(run claude -- --continue)"
expect "codex --continue resumes the last session" "0|codex resume --last" "$(run codex -- --continue)"
expect "opencode --continue" "0|opencode --continue" "$(run opencode -- --continue)"

expect "arguments are passed through" "0|codex --version" "$(run codex -- --version)"
expect "--continue keeps the arguments after it" "0|codex resume --last -x" "$(run codex -- --continue -x)"
expect "--continue is only special as the first argument" "0|claude -p --continue" "$(run claude -- -p --continue)"

contains "no agent installed fails clearly" 1 "no agent" "$(run "" --)"
contains "two agents without SANDBOX_AGENT fail and name both" 1 "claude and codex are both installed" "$(run "claude codex" --)"
expect "SANDBOX_AGENT picks one of two" "0|codex " "$(run "claude codex" SANDBOX_AGENT=codex --)"
expect "SANDBOX_AGENT=codex gets the codex --continue" "0|codex resume --last" "$(run "claude codex" SANDBOX_AGENT=codex -- --continue)"
expect "SANDBOX_AGENT as a full path still gets the codex --continue" "0|codex resume --last" "$(run codex SANDBOX_AGENT=@DIR@/codex -- --continue)"
contains "SANDBOX_AGENT naming a missing binary fails" 127 "not found" "$(run claude SANDBOX_AGENT=nope --)"

[ "$fail" = 0 ] && echo "all launcher checks passed" || echo "some launcher checks failed"
exit "$fail"
