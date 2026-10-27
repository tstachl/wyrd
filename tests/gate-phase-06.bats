#!/usr/bin/env bats
# shellcheck disable=SC2034
# (SSH/SSH_OPTS are read indirectly by on_o/on_n from
# tests/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate-phase-06.bats — gate-path phase 6: relay-partition conflict.
#
# Ports tests/alpha-microvm.sh phase 6 (microvm 10): the rename half
# runs pre-partition, the host partitions the relay's tap only after
# both rename-done files land, then heals after both conflict
# writes. Done-file rendezvous rides the shared state root,
# unaffected by the partition. Runs as the build user under
# tests/gate-entry.sh, after phases 1-5 in the same bats
# invocation. GMD/GMC/OD/OC are recomputed here (bats runs each
# @test in its own process, so no shell variable crosses phases).
# Partition is the host capability GATE_PARTITION_BIN (mandatory);
# every failure path between down and up re-ups explicitly (never
# via an EXIT trap, which would swallow the verdict) so a failure
# never strands the relay unreachable.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN PEER_O PEER_N RELAY_URL GATE_PARTITION_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY GATE_BATS_LIB CORE_ONLY GUEST_ENV; do
    [ -n "${!v+x}" ] || { echo "gate-phase-06: $v is not set (run through tests/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/mnt/wyrd-state/run/e2e-env.sh" ] || { echo "gate-phase-06: GUEST_ENV=$GUEST_ENV is not the guest-visible env file" >&2; return 1; }
  source "$BATS_TEST_DIRNAME/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate-entry.sh (gate task)" --check-owner-pid
  SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes"
  SSH="ssh $SSH_OPTS"
}

@test "rename breaks a held handle, then both sides commit under partition" {
  mkdir -p "$RUN/logs"
  OD="$GUEST_RUN/drives/owner"
  OC="$GUEST_RUN/creds/owner"
  GMD="$GUEST_RUN/drives/member-n"
  GMC="$GUEST_RUN/creds/member-n"
  rm -f "$RUN"/conflict-rename-member-ready "$RUN"/conflict-rename-owner-done \
    "$RUN"/conflict-rename-member-done "$RUN"/conflict-partitioned \
    "$RUN"/conflict-owner-written "$RUN"/conflict-member-written \
    "$RUN"/conflict-healed "$RUN"/conflict-owner-done "$RUN"/conflict-member-done \
    "$RUN"/rename-stale.err "$RUN/logs"/conflict-eio-write-owner.stderr \
    "$RUN/logs"/conflict-eio-write-member.stderr \
    "$RUN/logs"/conflict-reread-owner-1.stderr \
    "$RUN/logs"/conflict-reread-owner-2.stderr \
    "$RUN/logs"/conflict-reread-member-1.stderr \
    "$RUN/logs"/conflict-reread-member-2.stderr
  # Legs run backgrounded with output to log files (not `run`): their
  # output is voluminous and the failure names the log, as in the
  # legacy phase.
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh conflict-member $GMD $GMC $RELAY_URL" \
    >"$RUN/logs/leg-conflict-member.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh conflict-owner $OD $OC $RELAY_URL" \
    >"$RUN/logs/leg-conflict-owner.out" 2>&1 &
  LEG_O=$!
  wait_conflict_file 300 "$RUN/conflict-rename-owner-done" \
    "owner rename leg never finished (see logs/leg-conflict-owner.out)" "$LEG_O" \
    || return 1
  wait_conflict_file 120 "$RUN/conflict-rename-member-done" \
    "member stale-handle probe never finished (see logs/leg-conflict-member.out)" "$LEG_N" \
    || return 1
  # Best-effort heal, called explicitly on every failure path below:
  # never an EXIT trap — a test-installed EXIT trap replaces bats'
  # own result reporting, and a failure inside the trap window then
  # vanishes with no verdict (the suite keeps going and the plan
  # undercounts). heal_tap itself never fails the test.
  heal_tap() {
    if [[ -n "${GATE_PARTITION_BIN:-}" ]]; then
      "$GATE_PARTITION_BIN" tap-r up 2>/dev/null || true
    fi
  }
  host_partition tap-r down || { heal_tap; echo "  FAIL: host could not partition tap-r" >&2; return 1; }
  touch "$RUN/conflict-partitioned"
  wait_conflict_file 180 "$RUN/conflict-owner-written" \
    "owner never committed under partition (see logs/leg-conflict-owner.out)" "$LEG_O" \
    || { heal_tap; return 1; }
  wait_conflict_file 180 "$RUN/conflict-member-written" \
    "member never committed under partition (see logs/leg-conflict-member.out)" "$LEG_N" \
    || { heal_tap; return 1; }
  host_partition tap-r up || { heal_tap; echo "  FAIL: host could not heal tap-r" >&2; return 1; }
  touch "$RUN/conflict-healed"
  wait "$LEG_N" || die "conflict member leg failed (see logs/leg-conflict-member.out)"
  wait "$LEG_O" || die "conflict owner leg failed (see logs/leg-conflict-owner.out)"
}
