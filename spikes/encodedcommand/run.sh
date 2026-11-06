#!/usr/bin/env bash
# Spike: wincred via powershell.exe -EncodedCommand, secret on stdin only.
set -u
cd "$(dirname "$0")"
BODY=$(cat wincred-body.ps1)
ID=$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')
T="cbox-spike/$ID"
PASS=0; FAIL=0
ok()  { echo "PASS  $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL  $1${2:+ -- $2}"; FAIL=$((FAIL+1)); }

# encode OP TARGET [PAD_BYTES] -> base64(UTF-16LE(script with $Op/$Target prepended))
encode() {
  { printf "\$Op='%s'; \$Target='%s'\n" "$1" "$2"
    printf '%s\n' "$BODY"
    [ "${3:-0}" -gt 0 ] && head -c "$3" /dev/zero | tr '\0' 'x' | sed 's/^/# /'
  } | iconv -f UTF-8 -t UTF-16LE | base64 -w0
}
# ps OP TARGET [PAD]: stdin is passed through; stderr \r stripped to $ERR
ps() {
  local enc; enc=$(encode "$@")
  powershell.exe -NoProfile -NonInteractive -EncodedCommand "$enc" 2> >(tr -d '\r' >"$ERRF")
}
ERRF=$(mktemp)
cleanup() { powershell.exe -NoProfile -NonInteractive -EncodedCommand "$(encode del "$T")" </dev/null >/dev/null 2>&1; rm -f "$ERRF"; }
trap cleanup EXIT

echo "== environment"
command -v powershell.exe >/dev/null || { echo "FAIL  powershell.exe not on PATH (interop off?)"; exit 1; }
powershell.exe -NoProfile -NonInteractive -Command '$PSVersionTable.PSVersion.ToString(); $ExecutionContext.SessionState.LanguageMode' </dev/null 2>&1 | tr -d '\r' | sed 's/^/      /'
echo "      (expect 5.1.x and FullLanguage; ConstrainedLanguage breaks Add-Type)"
echo "== checks"

# 1. set via stdin, get back, byte-for-byte (compare by hash; never print)
S1=$'sk-spike-ascii-1234'
printf '%s' "$S1" | ps set "$T" >/dev/null; rc=$?
[ $rc -eq 0 ] && ok "1a set exits 0 (secret on stdin)" || bad "1a set exits 0" "rc=$rc $(head -c 300 "$ERRF")"
GOT=$(ps get "$T" </dev/null; echo x); GOT=${GOT%x}
[ "$GOT" = "$S1" ] && ok "1b get returns exactly what was set" || bad "1b round-trip" "lengths ${#GOT} vs ${#S1}"

# 2. non-ASCII UTF-8 round-trip
S2=$'päss-€-日本-\U0001F511'
printf '%s' "$S2" | ps set "$T" >/dev/null
GOT=$(ps get "$T" </dev/null; echo x); GOT=${GOT%x}
[ "$GOT" = "$S2" ] && ok "2 non-ASCII UTF-8 round-trips" || bad "2 non-ASCII round-trip" "lengths ${#GOT} vs ${#S2}"

# 3. missing target: "Element not found" on stderr, exit 1
ps get "cbox-spike/missing-$ID" </dev/null >/dev/null; rc=$?
grep -q "Element not found" "$ERRF" && ok "3a missing target says 'Element not found'" || bad "3a message" "$(head -c 300 "$ERRF")"
[ $rc -eq 1 ] && ok "3b missing target exits 1" || bad "3b exit code" "rc=$rc"

# 4. size: 2560 bytes ok, 2561 rejected (script cap) and exits 1
head -c 2560 /dev/zero | tr '\0' 'a' | ps set "$T" >/dev/null; rc=$?
[ $rc -eq 0 ] && ok "4a 2560-byte secret saves" || bad "4a 2560 bytes" "rc=$rc $(head -c 200 "$ERRF")"
head -c 2561 /dev/zero | tr '\0' 'a' | ps set "$T" >/dev/null; rc=$?
[ $rc -eq 1 ] && ok "4b 2561-byte secret rejected, exit 1" || bad "4b 2561 bytes" "rc=$rc"

# 5. a much larger encoded command still runs (headroom over the real ~7 KB)
LEN=$(encode get "$T" | wc -c)
printf '%s' "$S1" | ps set "$T" >/dev/null
GOT=$(ps get "$T" 8000 </dev/null; echo x); GOT=${GOT%x}
BIG=$(encode get "$T" 8000 | wc -c)
[ "$GOT" = "$S1" ] && ok "5 encoded command works at $BIG chars (real size $LEN; Windows caps a command line at 32767)" || bad "5 large command" "size $BIG: $(head -c 200 "$ERRF")"

# 6. the secret never appears in any process command line while set runs
M="MARKER-$ID"
( sleep 4; printf '%s' "$M" ) | ps set "$T" >/dev/null &
SP=$!
sleep 2
CL=$(powershell.exe -NoProfile -NonInteractive -Command 'Get-CimInstance Win32_Process | ForEach-Object { $_.CommandLine }' </dev/null 2>/dev/null | tr -d '\r')
wait $SP
if grep -q "$M" <<<"$CL"; then bad "6 secret visible in a Windows process command line"
elif grep -q "EncodedCommand" <<<"$CL"; then ok "6 secret not in any command line (the set process was visible)"
else bad "6 inconclusive: set process not seen in the process list" "(try again; or Get-CimInstance is blocked)"; fi

# 7. delete, then delete-missing exits 1
ps del "$T" </dev/null >/dev/null; rc=$?
[ $rc -eq 0 ] && ok "7a delete exits 0" || bad "7a delete" "rc=$rc $(head -c 200 "$ERRF")"
ps del "$T" </dev/null >/dev/null; rc=$?
[ $rc -eq 1 ] && grep -q "Element not found" "$ERRF" && ok "7b delete of missing exits 1 with 'Element not found'" || bad "7b delete missing" "rc=$rc"

# 8. latency
s=$(date +%s%N); printf x | ps set "$T" >/dev/null; e=$(date +%s%N)
echo "      one set call: $(( (e - s) / 1000000 )) ms"

echo "== $PASS passed, $FAIL failed"
[ $FAIL -eq 0 ]
