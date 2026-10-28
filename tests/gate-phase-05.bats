#!/usr/bin/env bats
# shellcheck disable=SC2034
# (SSH/SSH_OPTS are read indirectly by on_o/on_n from
# tests/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate-phase-05.bats — gate-path phase 5: fetch plane.
#
# Ports legacy microVM phase 9: blocking open,
# bounded EIO, recovery, and dedupe hold across hosts
# (fetch-member/fetch-owner legs). Runs as the build user under
# tests/gate-entry.sh, after phases 1-4 in the same bats
# invocation. GMD/GMC/OD/OC are recomputed here (bats runs each
# @test in its own process, so no shell variable crosses phases).
# The phase-4 dedupe snapshot ($RUN/seen-after-restart) needs no
# host handling here: the fetch-member leg reads it in-guest via
# the shared state ($E2E_ROOT/seen-after-restart) itself.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN PEER_O PEER_N RELAY_URL WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY GATE_BATS_LIB CORE_ONLY GUEST_ENV; do
    [ -n "${!v+x}" ] || { echo "gate-phase-05: $v is not set (run through tests/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/mnt/wyrd-state/run/e2e-env.sh" ] || { echo "gate-phase-05: GUEST_ENV=$GUEST_ENV is not the guest-visible env file" >&2; return 1; }
  source "$BATS_TEST_DIRNAME/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate-entry.sh (gate task)" --check-owner-pid
  SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes"
  SSH="ssh $SSH_OPTS"
}

@test "blocking open, bounded EIO, recovery, and dedupe hold across hosts" {
  mkdir -p "$RUN/logs"
  OD="$GUEST_RUN/drives/owner"
  OC="$GUEST_RUN/creds/owner"
  GMD="$GUEST_RUN/drives/member-n"
  GMC="$GUEST_RUN/creds/member-n"
  rm -f "$RUN/member-cold-done" "$RUN/member-listed-done" \
    "$RUN/member-scratch-done" "$RUN/owner-stopped" "$RUN/member-fetch-done" \
    "$RUN/member-probed-done" "$RUN/owner-converged-done" \
    "$RUN/cold-2.got" "$RUN/stale-1.err" "$RUN/stale-2.err" \
    "$RUN/never-announced.err" "$RUN/owner-back" \
    "$RUN/member-recovered-done" "$RUN/stale-2-recovered.got"
  # Legs run backgrounded with output to log files (not `run`): their
  # output is voluminous and the failure names the log, as in the
  # legacy phase.
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh fetch-member $GMD $GMC $RELAY_URL" \
    >"$RUN/logs/leg-fetch-member.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh fetch-owner $OD $OC $RELAY_URL" \
    >"$RUN/logs/leg-fetch-owner.out" 2>&1 &
  LEG_O=$!
  wait "$LEG_N" || { echo "fetch member leg failed (see logs/leg-fetch-member.out)"; false; }
  wait "$LEG_O" || { echo "fetch owner leg failed (see logs/leg-fetch-owner.out)"; false; }
}
