#!/usr/bin/env bash
# tests/e2e-helpers.bash — shared ssh plumbing for the e2e suites.
#
# Sourced, never executed: functions only, no side effects at source
# time. Sourced by tests/alpha-microvm.sh (dev loop and legacy gate
# path) and by the gate-path bats phases (tests/gate-phase-*.bats),
# so both paths dial guests identically; the environment (direct
# exec vs. wrappers, partition primitive) is what differs, and that
# lives in the gate-owned bats helpers, not here.
#
# Requires from the environment: SSH (the ssh command line with key
# and options), PEER_O, PEER_N (user@host endpoints).
# Provides task constants with overrideable defaults: GUEST_TESTS
# (baked suite path in guests), GUEST_ENV (guest env file path),
# GUEST_RUN (guest run-tree path).
# Provides host capability shims: as_guest (de-escalated host wyrd
# invocations) and hand_to_guest (share handover for created
# credential material). Both are environment-driven: when the
# wrapper bins are set (dev loop) they delegate, otherwise (task
# path, already running as the owner) they exec directly / no-op.
# host_partition (tap surgery for fault phases) is mandatory and
# fatal: the host must set GATE_PARTITION_BIN. It lives here (not
# in the gate-owned bats helpers) because both paths name the tap
# and direction identically; only the binary behind the env var
# differs (dev-partition.sh vs the host-owned tap primitive).
#
# Also provides the harness failure primitive (die) and the
# host-driven fault rendezvous waiter (wait_conflict_file), shared
# so both paths fail identically. Note the semantic shift: in the
# legacy script die aborts the whole run; in bats each @test runs
# in its own process, so die aborts only the current test and later
# phases cascade-fail on the missing state.

# die <msg>: fail with the harness FAIL line. Legacy path: aborts
# the script. Gate path: aborts the current @test.
die() { echo "  FAIL: $1" >&2; exit 1; }

: "${GUEST_TESTS:=/etc/wyrd-tests}"
: "${GUEST_ENV:=/mnt/wyrd-state/e2e-env.sh}"
: "${GUEST_RUN:=/mnt/wyrd-state/run}"

# on_o <cmd...>: run the command on peer-o, carrying the run token
# (and the explicit-unwrapped flag) so the guest-side guard can
# verify the call came through a wrapper.
on_o() {
  if [[ -z "${SSH:-}" || -z "${PEER_O:-}" ]]; then echo "e2e-helpers: on_o requires SSH and PEER_O" >&2; return 1; fi
  $SSH "$PEER_O" "WYRD_E2E_TOKEN=${WYRD_E2E_TOKEN:-}" "WYRD_E2E_UNWRAPPED=${WYRD_E2E_UNWRAPPED:-0}" "$@"
}

# on_n <cmd...>: run the command on peer-n. Same contract as on_o.
on_n() {
  if [[ -z "${SSH:-}" || -z "${PEER_N:-}" ]]; then echo "e2e-helpers: on_n requires SSH and PEER_N" >&2; return 1; fi
  $SSH "$PEER_N" "WYRD_E2E_TOKEN=${WYRD_E2E_TOKEN:-}" "WYRD_E2E_UNWRAPPED=${WYRD_E2E_UNWRAPPED:-0}" "$@"
}

# as_guest <cmd...>: run a host wyrd invocation de-escalated when the
# wrapper is provided (dev loop drops to uid 1000); direct exec in
# the task path, where the suite already runs as the owner.
as_guest() {
  if [[ -n "${GATE_AS_GUEST_BIN:-}" ]]; then
    "$GATE_AS_GUEST_BIN" "$@"
  else
    "$@"
  fi
}

# hand_to_guest <paths...>: hand created credential material to the
# guest owner (dev loop hands ownership to uid 1000); a no-op in the
# task path, where files arrive correctly owned.
hand_to_guest() {
  if [[ -n "${GATE_HAND_TO_GUEST_BIN:-}" ]]; then
    "$GATE_HAND_TO_GUEST_BIN" "$@"
  else
    :
  fi
}

# host_partition <tap> down|up: perform tap surgery for fault
# phases. Mandatory: refuses when the host did not provide
# GATE_PARTITION_BIN.
host_partition() { # <tap> down|up
  [[ -n "${GATE_PARTITION_BIN:-}" ]] \
    || die "partition capability not provided (host must set GATE_PARTITION_BIN)"
  "$GATE_PARTITION_BIN" "$@"
}

# wait_conflict_file <seconds> <file> <what> <leg-pid>: shared
# rendezvous waiter for host-driven fault blocks (partition, crash):
# fail fast when the awaited leg exits early instead of burning the
# whole budget on a corpse. Returns 0 on rendezvous, 1 on timeout or
# corpse — never exits: callers die (or heal-then-die) themselves, so
# every failure surfaces as a verdict. (An EXIT trap must never be
# used for the heal: it replaces bats' own result reporting and turns
# failures into missing verdicts with a passing-looking count.)
wait_conflict_file() { # <seconds> <file> <what> <leg-pid>
  local budget="$1" f="$2" what="$3" leg="$4" i
  for ((i = 0; i < budget * 2; i++)); do
    [[ -f "$f" ]] && return 0
    # A dead leg never touches its file: fail fast instead of burning
    # the whole budget on a corpse. The pid is per-file (the sibling
    # leg may already have finished fine); $what names the awaited
    # rendezvous and its leg log.
    kill -0 "$leg" 2>/dev/null || { echo "  FAIL: $what (leg exited early)" >&2; return 1; }
    sleep 0.5
  done
  echo "  FAIL: $what" >&2
  return 1
}
