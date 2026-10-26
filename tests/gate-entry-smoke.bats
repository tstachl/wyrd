#!/usr/bin/env bats
# tests/gate-entry-smoke.bats — first gate-path suite: helpers load and
# both peers execute the shared wyrd binary.
#
# Runs as the build user under tests/gate-entry.sh, which assembles
# every variable used below. Not runnable directly: without the entry
# the setup refuses (fail fast, not skip — a green run with skipped
# tests would pretend coverage). Odin-only: the peers are real VMs.
#
# Later phases migrate here file by file; this smoke file proves the
# entry contract (env assembly, token guard, ssh reachability) before
# any phase logic lands.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN NAK_BIN PEER_O PEER_N WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY GATE_BATS_LIB; do
    [ -n "${!v:-}" ] || { echo "gate-entry-smoke: $v is not set (run through tests/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  load "$GATE_BATS_LIB/partition"
  load "$GATE_BATS_LIB/guest"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate-entry.sh (gate task)" --check-owner-pid
  SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes"
}

@test "gate helpers load and the guest helper runs directly" {
  declare -F gate_partition >/dev/null
  declare -F gate_as_guest >/dev/null
  run gate_as_guest true
  [ "$status" -eq 0 ]
}

@test "peer-o executes WYRD_BIN from the shared store" {
  # Hardcoded 120s bound on top of ssh's own ConnectTimeout: a hung
  # guest fails the phase loudly instead of wedging the unit.
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$PEER_O" "test -x $WYRD_BIN"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
}

@test "peer-n executes WYRD_BIN from the shared store" {
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$PEER_N" "test -x $WYRD_BIN"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
}
