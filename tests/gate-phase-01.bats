#!/usr/bin/env bats
# shellcheck disable=SC2034
# (SSH/SSH_OPTS are read indirectly by on_o/on_n from
# tests/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate-phase-01.bats — gate-path phase 1: shared core on peer-o.
#
# Ports tests/alpha-microvm.sh phase 1 (microvm 1-5): the shared-core
# Lima steps run on peer-o. Runs as the build user under
# tests/gate-entry.sh; Odin-only (dials a real peer).
#
# Later phases follow this file's shape: setup (guard + helpers),
# one @test per phase step, shared ssh plumbing from
# tests/e2e-helpers.bash. Step selection (--step prefix closure)
# arrives with the fuller migration; this file always runs phase 1,
# which every prefix includes.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN PEER_O WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY GATE_BATS_LIB CORE_ONLY GUEST_ENV; do
    [ -n "${!v+x}" ] || { echo "gate-phase-01: $v is not set (run through tests/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/mnt/wyrd-state/e2e-env.sh" ] || { echo "gate-phase-01: GUEST_ENV=$GUEST_ENV is not the guest-visible env file" >&2; return 1; }
  source "$BATS_TEST_DIRNAME/e2e-helpers.bash"
  e2e_require_wrapped "$STATE_DIR/e2e-env.sh" "./tests/gate-entry.sh (gate task)" --check-owner-pid
  SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes"
  SSH="ssh $SSH_OPTS"
}

@test "shared core steps run green on peer-o" {
  # Same invocation as the legacy phase 1; CORE_ONLY arrives from the
  # entry (default 1,2,3,4,5, mirroring alpha-microvm.sh).
  run on_o "E2E_ENV_FILE=$GUEST_ENV E2E_ONLY_STEP=$CORE_ONLY bash $GUEST_TESTS/alpha-lima.sh"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
}
