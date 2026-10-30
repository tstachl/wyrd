#!/usr/bin/env bash
# tests/test-supervise.sh — the supervisor wrapper maps wait statuses
# onto the exit code the supervisor's policy wants, and logs
# readiness without ever killing a live child over it. Pure
# process-level checks: stub children, real waitpid, no supervisor,
# no drive.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WRAP="$ROOT/nix/wyrd-supervise.py"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

command -v python3 >/dev/null || { echo "SKIP: python3 is required"; exit 0; }

run_wrap() {
    # code=0; run_wrap <restart-status> <posture-path> <timeout> -- <cmd...>
    local status="$1" posture="$2" timeout="$3"
    shift 3
    python3 "$WRAP" --restart-status "$status" --posture-path "$posture" \
        --readiness-timeout "$timeout" -- "$@"
}

# Both flag spellings map identically: the module emits
# `--flag=value`, the shell form is `--flag value`.
code=0; run_wrap 1 "$ROOT/no-such-posture" 0 python3 -c 'import sys; sys.exit(78)' || code=$?
[ "$code" = "78" ] || die "exit 78 maps to 78 under systemd, got $code"
python3 "$WRAP" --restart-status=1 --posture-path="$ROOT/no-such-posture" --readiness-timeout=0 -- python3 -c 'import sys; sys.exit(78)' >/dev/null 2>&1 || code=$?
[ "$code" = "78" ] || die "equals-form flags map identically, got $code"
code=0; run_wrap 0 "$ROOT/no-such-posture" 0 python3 -c 'import sys; sys.exit(78)' || code=$?
[ "$code" = "78" ] || die "exit 78 maps to 78 under launchd, got $code"
pass "the lock code is terminal under both supervisors"

# Clean and runtime exits map to the restart-worthy status verbatim.
code=0; run_wrap 1 "$ROOT/no-such-posture" 0 python3 -c 'import sys; sys.exit(0)' || code=$?
[ "$code" = "1" ] || die "exit 0 maps to 1 for systemd"
code=0; run_wrap 0 "$ROOT/no-such-posture" 0 python3 -c 'import sys; sys.exit(0)' || code=$?
[ "$code" = "0" ] || die "exit 0 maps to 0 for launchd"
code=0; run_wrap 1 "$ROOT/no-such-posture" 0 python3 -c 'import sys; sys.exit(3)' || code=$?
[ "$code" = "1" ] || die "exit 3 maps to 1 for systemd"
code=0; run_wrap 0 "$ROOT/no-such-posture" 0 python3 -c 'import sys; sys.exit(3)' || code=$?
[ "$code" = "0" ] || die "exit 3 maps to 0 for launchd"
pass "clean and runtime exits map to the restart-worthy status"

# A signal death is not an exit: the wrapper reports it restart-worthy
# instead of letting 128+N masquerade as a code.
code=0; run_wrap 1 "$ROOT/no-such-posture" 0 sh -c 'kill -KILL $$' || code=$?
[ "$code" = "1" ] || die "SIGKILL maps to 1 for systemd"
code=0; run_wrap 0 "$ROOT/no-such-posture" 0 sh -c 'kill -KILL $$' || code=$?
[ "$code" = "0" ] || die "SIGKILL maps to 0 for launchd"
pass "a signal death maps to the restart-worthy status"

# Readiness is a log line: a fresh sidecar logs ready, a missing one
# warns after the bound, and neither changes the mapped exit.
fresh="$ROOT/.supervise-test-fresh"
touch "$fresh"
out=$(code=0; run_wrap 1 "$fresh" 30 python3 -c 'import sys; sys.exit(0)' 2>&1) || code=$?
[ "$code" = "1" ] || die "readiness must not change the mapped exit"
echo "$out" | grep -q "ready (posture fresh" || die "a fresh sidecar logs ready, got: $out"
rm -f "$fresh"
out=$(code=0; run_wrap 1 "$ROOT/no-such-posture" 1 python3 -c 'import time; time.sleep(3)' 2>&1) || code=$?
[ "$code" = "1" ] || die "a missing sidecar must not change the mapped exit"
echo "$out" | grep -q "supervising without a readiness line" \
    || die "a missing sidecar warns after the bound, got: $out"
pass "readiness logs without touching the exit contract"

# A broken invocation fails fast instead of supervising nothing.
python3 "$WRAP" --restart-status 1 >/dev/null 2>&1 || code=$?
[ "$code" = "2" ] || die "a missing command exits 2"
pass "usage errors exit 2"

echo "supervise: $PASS checks passed"
