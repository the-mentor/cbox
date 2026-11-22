#!/bin/sh
# Pre-approve ANTHROPIC_API_KEY in ~/.claude.json so the "Detected a custom
# API key" interactive prompt is skipped on every fresh boot of the ephemeral box.
# A failure must not stop claude from starting, but it is reported: otherwise the
# only sign is that prompt coming back, with no hint why.
if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
    python3 - <<'EOF' || true
import errno, json, os, shutil, sys, tempfile

def approve():
    key = os.environ.get('ANTHROPIC_API_KEY', '')
    if not key:
        return
    suffix = key[-20:]
    cfg = os.path.expanduser('~/.claude.json')
    with open(cfg) as f:
        c = json.load(f)
    approved = c.setdefault('customApiKeyResponses', {}).setdefault('approved', [])
    if suffix in approved:
        return
    approved.append(suffix)
    # Write a temp file next to cfg, then rename it over cfg: a failed write
    # (e.g. a full disk) leaves the original config untouched, not truncated.
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(cfg))
    try:
        with os.fdopen(fd, 'w') as f:
            json.dump(c, f, indent=2)
        os.chmod(tmp, os.stat(cfg).st_mode & 0o777)
        try:
            os.replace(tmp, cfg)
        except OSError as e:
            # A bind-mounted file cannot be renamed over: copy the finished temp
            # file into it in place instead.
            if e.errno not in (errno.EBUSY, errno.EXDEV, errno.EPERM):
                raise
            shutil.copyfile(tmp, cfg)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)

try:
    approve()
except Exception as e:
    sys.exit(f'claude-wrapper: could not pre-approve ANTHROPIC_API_KEY in ~/.claude.json '
             f'({type(e).__name__}: {e}); expect the custom API key prompt')
EOF
fi
exec claude-real "$@"
