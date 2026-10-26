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

: "${GUEST_TESTS:=/etc/wyrd-tests}"
: "${GUEST_ENV:=/mnt/wyrd-state/e2e-env.sh}"
: "${GUEST_RUN:=/mnt/wyrd-state/run}"

# on_o <cmd...>: run the command on peer-o, carrying the run token
# (and the explicit-unwrapped flag) so the guest-side guard can
# verify the call came through a wrapper.
on_o() { $SSH "$PEER_O" "WYRD_E2E_TOKEN=${WYRD_E2E_TOKEN:-}" "WYRD_E2E_UNWRAPPED=${WYRD_E2E_UNWRAPPED:-0}" "$@"; }

# on_n <cmd...>: run the command on peer-n. Same contract as on_o.
on_n() { $SSH "$PEER_N" "WYRD_E2E_TOKEN=${WYRD_E2E_TOKEN:-}" "WYRD_E2E_UNWRAPPED=${WYRD_E2E_UNWRAPPED:-0}" "$@"; }
