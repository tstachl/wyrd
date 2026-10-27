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
# The partition primitive stays out: it is mandatory and fatal
# (host must set GATE_PARTITION_BIN), and fault phases own it.

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
