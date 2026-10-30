#!/usr/bin/env bash
# tests/harness/test-drive-probe.sh — the switch-time conflict probe refuses
# the switch when a foreign process holds the drive, names the
# holder from the freshest sidecar, and rolls the switch when the
# holder is the service's own previous generation. Real flock locks,
# real sidecars, no supervisor.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PROBE="$ROOT/nix/drive/probe.py"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

command -v python3 >/dev/null || { echo "SKIP: python3 is required"; exit 0; }
command -v lsof >/dev/null || { echo "SKIP: lsof is required"; exit 0; }

sandbox() {
    d="$(python3 -c 'import tempfile; print(tempfile.mkdtemp(prefix="wyrd-probe-"))')"
    mkdir -p "$d/drive"
    touch "$d/drive/LOCK"
    printf '%s' "$d"
}

hold() {
    # hold <lockfile> <seconds>: exclusive flock in the background.
    python3 - "$1" "$2" <<'EOF' &
import fcntl, sys, time
fd = open(sys.argv[1], 'r')
fcntl.flock(fd, fcntl.LOCK_EX)
time.sleep(int(sys.argv[2]))
EOF
}

# A free drive proceeds silently.
D=$(sandbox)
code=0; out=$(python3 "$PROBE" "$D/drive" 0 2>&1) || code=$?
[ "$code" = "0" ] || die "a free drive proceeds, got $code: $out"
[ -z "$out" ] || die "a free drive prints nothing, got: $out"
pass "a free drive proceeds silently"

# A foreign holder refuses the switch with the drive path.
hold "$D/drive/LOCK" 10
holder=$!
sleep 1
code=0; out=$(python3 "$PROBE" "$D/drive" 0 2>&1) || code=$?
[ "$code" = "1" ] || die "a foreign holder refuses, got $code: $out"
echo "$out" | grep -q "refusing switch" || die "the refusal says so: $out"
echo "$out" | grep -q "$D/drive" || die "the refusal names the drive: $out"
echo "$out" | grep -q "never published" || die "an unpublished holder says so: $out"
pass "a foreign holder refuses with the drive path"
kill "$holder" 2>/dev/null || true
wait 2>/dev/null || true

# A holder with a fresh posture sidecar is named by instance.
hold "$D/drive/LOCK" 10
holder=$!
sleep 1
python3 - "$D/drive" <<'EOF'
import json, sys, time
now = int(time.time())
file = {
    "schema": 1,
    "instance": "0123456789abcdef0123456789abcdef",
    "serving_id": "aa",
    "started_unix": now - 10,
    "ready_unix": now - 5,
    "last_pass_unix": now,
    "last_pass_verdict": "ok",
    "last_pass_class": None,
    "last_pass_consecutive": None,
    "passes": 3,
    "errors": 0,
    "sent": 1,
    "uptime_secs": 12,
    "posture_unix": now,
}
open(sys.argv[1] + "/vault.posture", "w").write(json.dumps(file))
EOF
code=0; out=$(python3 "$PROBE" "$D/drive" 0 2>&1) || code=$?
[ "$code" = "1" ] || die "a publishing holder still refuses, got $code"
echo "$out" | grep -q "0123456789abcdef0123456789abcdef" \
    || die "the refusal names the holder instance: $out"
pass "a publishing holder is named by instance"
kill "$holder" 2>/dev/null || true
wait 2>/dev/null || true

# The service's own previous generation rolls the switch instead.
hold "$D/drive/LOCK" 10
holder=$!
sleep 1
code=0; out=$(python3 "$PROBE" "$D/drive" "$holder" 2>&1) || code=$?
[ "$code" = "0" ] || die "its own previous generation rolls, got $code: $out"
echo "$out" | grep -q "previous generation" || die "the roll says so: $out"
pass "its own previous generation rolls the switch"
kill "$holder" 2>/dev/null || true
wait 2>/dev/null || true

rm -rf "$D"
echo "drive-probe: $PASS checks passed"
