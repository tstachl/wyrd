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

FIXTURE="$(mktemp -d)"
CALLS="$FIXTURE/calls"
trap 'rm -rf "$FIXTURE"' EXIT

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
# Covers the gate suite, its helpers, the Lima definitions, and
# the microVM call sites (peer_wait/peer_signal are defined in Lima
# but called from the microVM legs).
HITS="$(grep -rn -E 'timeout +[0-9]+ +(guest_exec|on_o|on_n|guest_wyrd|guest_timeout|mint_guest_creds|wait_guest_file|host_partition|as_e2e|gate_partition|gate_as_guest|peer_wait|peer_signal)( |$|")' \
  "$ROOT/tests/gate" "$ROOT/tests/harness/e2e-helpers.bash" "$ROOT/tests/lima" "$ROOT/tests/microvm" || true)"
[[ -z "$HITS" ]] || die "timeout wraps a shell function (use guest_timeout or the transport binary):\n$HITS"
pass "no suite function runs under timeout"

# The helper itself, executed: argv shape, guest status propagation,
# and the bound. A stub transport records argv and exits 7; a
# sleeper provokes the 124.
if command -v timeout >/dev/null 2>&1; then
  # shellcheck source=/dev/null
  source "$ROOT/tests/harness/e2e-helpers.bash"
  STUB="$FIXTURE/guest-bin-stub"
  cat >"$STUB" <<'EOF'
#!/bin/sh
echo "stub $*" >>"$CALLS"
exit "$STUB_RC"
EOF
  chmod +x "$STUB"
  : >"$CALLS"
  rc=0
  GATE_GUEST_BIN="$STUB" CALLS="$CALLS" STUB_RC=7 guest_timeout 10 peer-o -- test -x /run/current-system/sw/bin/wyrd || rc=$?
  [[ "$rc" == 7 ]] || die "guest status did not propagate (got $rc)"
  grep -q "^stub exec peer-o -- test -x /run/current-system/sw/bin/wyrd$" "$CALLS" \
    || die "guest_timeout argv is not the transport invocation: $(cat "$CALLS")"
  pass "guest_timeout sends the transport invocation and propagates status"
  cat >"$STUB" <<'EOF'
#!/bin/sh
sleep 30
EOF
  chmod +x "$STUB"
  start="$(date +%s)"
  rc=0
  GATE_GUEST_BIN="$STUB" guest_timeout 2 peer-o -- true || rc=$?
  [[ "$rc" == 124 ]] || die "guest_timeout did not time out (got $rc)"
  elapsed=$(( $(date +%s) - start ))
  (( elapsed < 15 )) || die "guest_timeout overran its bound (${elapsed}s)"
  pass "guest_timeout enforces its bound"
  # The KILL grace: a client that ignores TERM is killed, and the
  # caller must see 137, not a hang. Expected wall time is secs +
  # grace (2 + 10 here); the bound below distinguishes that from a
  # missing -k (which would run out the sleep).
  cat >"$STUB" <<'EOF'
#!/bin/sh
trap '' TERM
sleep 30
EOF
  chmod +x "$STUB"
  start="$(date +%s)"
  rc=0
  GATE_GUEST_BIN="$STUB" guest_timeout 2 peer-o -- true || rc=$?
  [[ "$rc" == 137 ]] || die "KILL grace did not report 137 (got $rc)"
  elapsed=$(( $(date +%s) - start ))
  (( elapsed < 20 )) || die "KILL grace overran its bound (${elapsed}s)"
  pass "guest_timeout kills TERM-ignoring clients with 137"
else
  echo "  SKIP: timeout missing, helper execution omitted"
fi

echo "timeout-discipline: $PASS checks passed"
