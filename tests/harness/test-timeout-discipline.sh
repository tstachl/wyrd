#!/usr/bin/env bash
# tests/harness/test-timeout-discipline.sh — timeout wraps binaries,
# never suite shell functions.
#
# GNU timeout executes its command with execvp, so `timeout 120
# guest_exec ...` fails with 127 ("No such file or directory") even
# on a healthy guest — the intended bound is never applied and the
# suite fails closed on every call. The suite bounds agent calls
# through guest_timeout (which wraps the elevated gate transport
# binary directly) or by calling the transport under timeout.
# It also pins the two status helpers sourced from e2e-helpers.bash
# beside it: check_query_status (only 0 passes a bounded query) and
# guest_push_e2e (push-then-chown order, failed push skips the chown).
# Pure bash + grep, with live checks when timeout exists: run it
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
# Binaries (the transport via its elevation, python3, jq) are fine; the banned
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
  SUDO_STUB="$FIXTURE/sudo-stub"
  cat >"$SUDO_STUB" <<'EOF'
#!/bin/sh
echo "elevation $*" >>"${CALLS:-/dev/null}"
if [ "$1" = "-n" ]; then shift; fi
exec "$@"
EOF
  chmod +x "$SUDO_STUB"
  cat >"$STUB" <<'EOF'
#!/bin/sh
echo "stub $*" >>"$CALLS"
exit "$STUB_RC"
EOF
  chmod +x "$STUB"
  : >"$CALLS"
  rc=0
  E2E_SUDO_BIN="$SUDO_STUB" GATE_GUEST_BIN="$STUB" CALLS="$CALLS" STUB_RC=7 guest_timeout 10 peer-o -- test -x /run/current-system/sw/bin/wyrd || rc=$?
  [[ "$rc" == 7 ]] || die "guest status did not propagate (got $rc)"
  grep -q "^stub exec peer-o -- test -x /run/current-system/sw/bin/wyrd$" "$CALLS" \
    || die "guest_timeout argv is not the transport invocation: $(cat "$CALLS")"
  grep -q "^elevation -n " "$CALLS" \
    || die "guest_timeout does not elevate the transport: $(cat "$CALLS")"
  pass "guest_timeout sends the elevated transport invocation and propagates status"
  cat >"$STUB" <<'EOF'
#!/bin/sh
sleep 30
EOF
  chmod +x "$STUB"
  start="$(date +%s)"
  rc=0
  E2E_SUDO_BIN="$SUDO_STUB" GATE_GUEST_BIN="$STUB" guest_timeout 2 peer-o -- true || rc=$?
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
  E2E_SUDO_BIN="$SUDO_STUB" GATE_GUEST_BIN="$STUB" guest_timeout 2 peer-o -- true || rc=$?
  [[ "$rc" == 137 ]] || die "KILL grace did not report 137 (got $rc)"
  elapsed=$(( $(date +%s) - start ))
  (( elapsed < 20 )) || die "KILL grace overran its bound (${elapsed}s)"
  pass "guest_timeout kills TERM-ignoring clients with 137"
  # Query status vocabulary: only 0 passes. 124/137 name the timeout
  # (the phase treats both as inconclusive, never as clean); any
  # other nonzero status is a failed query, never a clean result.
  rc=0
  check_query_status 0 "probe query" "logs/probe.err" || rc=$?
  [[ "$rc" == 0 ]] || die "status 0 did not pass (got $rc)"
  for bad in 1 2 127; do
    rc=0
    out="$(check_query_status "$bad" "probe query" "logs/probe.err" 2>&1 || echo "status=$?")"
    [[ "$out" == *"failed (status $bad)"* ]] || die "status $bad did not fail as failed (got: $out)"
  done
  for to in 124 137; do
    rc=0
    out="$(check_query_status "$to" "probe query" "logs/probe.err" 2>&1 || echo "status=$?")"
    [[ "$out" == *"timed out (status $to)"* ]] || die "status $to did not fail as timed out (got: $out)"
  done
  pass "check_query_status passes only 0 and names timeouts"
  # Missing elevation grant: a sudo that refuses must surface as the
  # transport's failure, never as a pass or a hang. The stub sudo
  # exits 1 without running the guest binary (proves the grant is on
  # the call path); guest_timeout reports its status.
  cat >"$SUDO_STUB" <<'EOF'
#!/bin/sh
echo "refused $*" >>"${CALLS:-/dev/null}"
exit 1
EOF
  chmod +x "$SUDO_STUB"
  : >"$CALLS"
  rc=0
  E2E_SUDO_BIN="$SUDO_STUB" GATE_GUEST_BIN="$STUB" CALLS="$CALLS" guest_timeout 2 peer-o -- true || rc=$?
  [[ "$rc" == 1 ]] || die "refused elevation did not fail (got $rc)"
  grep -q "^refused -n " "$CALLS" || die "refused elevation never reached sudo: $(cat "$CALLS")"
  [ -z "$(grep "^stub " "$CALLS" || true)" ] || die "refused elevation still ran the guest binary"
  pass "missing elevation grant fails fast without touching the guest"
  # Handover order: the pushed file is handed to the suite user
  # after the push, and a failed push never reaches the chown.
  guest_push() { echo "push $*" >>"$CALLS"; return "${PUSH_RC:-0}"; }
  guest_exec() { echo "exec $*" >>"$CALLS"; return 0; }
  : >"$CALLS"
  PUSH_RC=0 guest_push_e2e peer-n /tmp/invitation /var/lib/wyrd-e2e/invitation
  [[ "$(cat "$CALLS")" == $'push peer-n /tmp/invitation /var/lib/wyrd-e2e/invitation\nexec peer-n chown e2e:users /var/lib/wyrd-e2e/invitation' ]] \
    || die "handover order is not push-then-chown: $(cat "$CALLS")"
  pass "guest_push_e2e hands the pushed file to the suite user"
  : >"$CALLS"
  rc=0
  PUSH_RC=3 guest_push_e2e peer-n /tmp/invitation /var/lib/wyrd-e2e/invitation || rc=$?
  [[ "$rc" == 3 ]] || die "failed push status did not propagate (got $rc)"
  grep -q "^push " "$CALLS" || die "failed push recorded nothing"
  [ -z "$(grep "^exec " "$CALLS" || true)" ] || die "failed push still ran the chown"
  pass "failed push skips the handover and propagates status"
else
  echo "  SKIP: timeout missing, helper execution omitted"
fi

echo "timeout-discipline: $PASS checks passed"
