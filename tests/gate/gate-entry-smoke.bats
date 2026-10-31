#!/usr/bin/env bats
# tests/gate/gate-entry-smoke.bats — first gate-path suite: helpers load and
# both peers execute the in-image binaries over the agent.
#
# Runs as the build user under tests/gate/gate-entry.sh, which assembles
# every variable used below. Not runnable directly: without the entry
# the setup refuses (fail fast, not skip — a green run with skipped
# tests would pretend coverage). Odin-only: the peers are real VMs.
#
# Later phases build on this file; this smoke file proves the entry
# contract (env assembly, token guard, agent reachability, in-image
# binaries) before any phase logic lands.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN NAK_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN; do
    [ -n "${!v:-}" ] || { echo "gate-entry-smoke: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  load "$GATE_BATS_LIB/partition"
  load "$GATE_BATS_LIB/guest"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

@test "gate helpers load and the guest helper runs directly" {
  declare -F gate_partition >/dev/null
  declare -F gate_as_guest >/dev/null
  declare -F guest_exec >/dev/null
  run gate_as_guest true
  [ "$status" -eq 0 ]
}

@test "peer-o executes the in-image binaries over the agent" {
  # Hardcoded 120s bound: a hung guest fails the phase loudly
  # instead of wedging the unit.
  run timeout 120 guest_exec peer-o -- test -x "$WYRD_BIN"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run timeout 120 guest_exec peer-o -- test -x "$NAK_BIN"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
}

@test "peer-n executes the in-image binaries over the agent" {
  run timeout 120 guest_exec peer-n -- test -x "$WYRD_BIN"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run timeout 120 guest_exec peer-n -- test -x "$NAK_BIN"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
}

@test "peers serve the run rendezvous channel" {
  # The boot service exposes each peer's signals dir from boot, so a
  # signal a leg writes stays fetchable after that leg exits. The
  # unit restarts on a 5 s delay, so the check retries briefly
  # instead of racing a restart; every cross-guest wait below
  # depends on the writer's server.
  for peer in peer-o peer-n; do
    active=""
    for ((i = 0; i < 30; i++)); do
      run guest_exec "$peer" -- systemctl is-active wyrd-rendezvous
      if [ "$status" -eq 0 ] && [ "$output" = "active" ]; then active=1; break; fi
      sleep 1
    done
    [[ -n "$active" ]] || { echo "rendezvous unit not active on $peer"; false; }
    run timeout 120 guest_exec "$peer" -- python3 -c 'import sys,urllib.request; urllib.request.urlopen("http://127.0.0.1:18770/", timeout=5)'
    [ "$status" -eq 0 ] || { echo "rendezvous channel down on $peer: $output"; false; }
  done
}
