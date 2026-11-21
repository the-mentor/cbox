#!/bin/sh
# Claude Code hook: hand the event to cbox on the host, which runs iTerm2's
# cc-status there (see cbox/src/hookfwd.rs). The event travels back as a hook
# terminalSequence, OSC 777 "cbox-hook" carrying the base64 event JSON; cbox
# strips it out of the terminal stream. Claude Code caps a terminalSequence
# at 4096 bytes, so drop tool_response and trim long strings first, falling
# back to just the fields that name the event.
#
# Only in iTerm2 (cbox forwards TERM_PROGRAM/ITERM_SESSION_ID per session),
# the one terminal with a host command to feed; elsewhere this does nothing.
# CLAUDE_ITERM2_INTEGRATION=0 turns it off.

[ "${CLAUDE_ITERM2_INTEGRATION:-1}" = 0 ] && exit 0
[ "${TERM_PROGRAM:-}" = iTerm.app ] || [ -n "${ITERM_SESSION_ID:-}" ] || exit 0

event=$(jq -c 'del(.tool_response) | walk(if type == "string" then .[:160] else . end)') || exit 0
b64=$(printf '%s' "$event" | base64 | tr -d '\n')
if [ "${#b64}" -gt 3900 ]; then
    event=$(printf '%s' "$event" | jq -c '{hook_event_name, session_id, tool_name, notification_type}')
    b64=$(printf '%s' "$event" | base64 | tr -d '\n')
fi
printf '{"terminalSequence":"\\u001b]777;cbox-hook;%s\\u0007"}\n' "$b64"
