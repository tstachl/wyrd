#!/usr/bin/env bash
# tests/harness/test-timeout-discipline.sh — timeout wraps binaries,
# never suite shell functions.
#
# GNU timeout executes its command with execvp, so `timeout 120
# guest_exec ...` fails with 127 ("No such file or directory") even
# on a healthy guest — the intended bound is never applied and the
# suite fails closed on every call. The suite bounds agent calls
# through guest_timeout (which wraps the gate transport binary
# directly) or by calling "$GATE_GUEST_BIN" under timeout. Pure bash
# + grep, with one live mechanism check when timeout exists: run it
# anywhere with `bash tests/harness/test-timeout-discipline.sh`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

# The mechanism, pinned: a shell function under timeout exits 127.
if command -v timeout >/dev/null 2>&1; then
  probe_fn() { echo ran; }
  rc=0
  timeout 5 probe_fn >/dev/null 2>&1 || rc=$?
  [[ "$rc" == 127 ]] || die "timeout on a shell function did not exit 127 (got $rc)"
  pass "timeout cannot execute shell functions (mechanism pinned)"
else
  echo "  SKIP: timeout missing, mechanism check omitted"
fi

# The ban: no suite function under timeout in host-executed code.
# Binaries ("$GATE_GUEST_BIN", python3, jq) are fine; the banned
# names are the suite's own functions, which only resolve in-shell.
HITS="$(grep -rn -E 'timeout +[0-9]+ +(guest_exec|on_o|on_n|guest_wyrd|guest_timeout|mint_guest_creds|wait_guest_file|host_partition|as_e2e|gate_partition|gate_as_guest|peer_wait|peer_signal)( |$|")' \
  "$ROOT/tests/gate" "$ROOT/tests/harness/e2e-helpers.bash" || true)"
[[ -z "$HITS" ]] || die "timeout wraps a shell function (use guest_timeout or the transport binary):\n$HITS"
pass "no suite function runs under timeout"

echo "timeout-discipline: $PASS checks passed"
