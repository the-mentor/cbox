#!/bin/sh
# Pre-approve ANTHROPIC_API_KEY in ~/.claude.json so the "Detected a custom
# API key" interactive prompt is skipped on every fresh boot of the ephemeral box.
if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
    python3 - <<'EOF' 2>/dev/null || true
import json, os, tempfile
key = os.environ.get('ANTHROPIC_API_KEY', '')
if key:
    suffix = key[-20:]
    cfg = os.path.expanduser('~/.claude.json')
    with open(cfg) as f:
        c = json.load(f)
    r = c.setdefault('customApiKeyResponses', {})
    a = r.setdefault('approved', [])
    if suffix not in a:
        a.append(suffix)
        # Write a temp file next to cfg, then rename it over cfg: a failed write
        # (e.g. a full disk) leaves the original config untouched, not truncated.
        fd, tmp = tempfile.mkstemp(dir=os.path.dirname(cfg))
        try:
            with os.fdopen(fd, 'w') as f:
                json.dump(c, f, indent=2)
            os.chmod(tmp, os.stat(cfg).st_mode & 0o777)
            os.replace(tmp, cfg)
        except BaseException:
            os.unlink(tmp)
            raise
EOF
fi
exec claude-real "$@"
