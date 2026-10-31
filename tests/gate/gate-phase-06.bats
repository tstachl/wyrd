#!/usr/bin/env bats
# shellcheck disable=SC2034
# (GATE_GUEST_BIN and friends are read indirectly by on_o/on_n from
# tests/harness/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate/gate-phase-06.bats — gate-path phase 6: relay-partition conflict.
#
# Ports the legacy rename-conflict leg: the rename half
# runs pre-partition, the host partitions the relay instance only
# after both rename-done signals land, then heals after both conflict
# writes. Signal rendezvous rides the peer HTTP channel, unaffected
# by the partition (bridge traffic, not relay traffic). Runs as the
# tests/gate/gate-entry.sh, after phases 1-5 in the same bats
# invocation. GMD/GMC/OD/OC are recomputed here (bats runs each
# @test in its own process, so no shell variable crosses phases).
# Partition is the host capability GATE_PARTITION_BIN (mandatory);
# every failure path between down and up re-ups explicitly (never
# via an EXIT trap, which would swallow the verdict) so a failure
# never strands the relay unreachable.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN GATE_PARTITION_BIN CORE_ONLY GUEST_ENV RELAY_URL; do
    [ -n "${!v+x}" ] || { echo "gate-phase-06: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/var/lib/wyrd-e2e/e2e-env.sh" ] || { echo "gate-phase-06: GUEST_ENV=$GUEST_ENV is not the guest env file" >&2; return 1; }
  load "$GATE_BATS_LIB/guest"
  load "$GATE_BATS_LIB/partition"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

@test "rename breaks a held handle, then both sides commit under partition" {
  mkdir -p "$RUN/logs"
  OD="$GUEST_RUN/drives/owner"
  OC="$GUEST_RUN/creds/owner"
  GMD="$GUEST_RUN/drives/member-n"
  GMC="$GUEST_RUN/creds/member-n"
  guest_exec peer-o -- rm -f "$GUEST_RUN"/signals/conflict-rename-owner-done \
    "$GUEST_RUN"/signals/conflict-partitioned \
    "$GUEST_RUN"/signals/conflict-owner-written \
    "$GUEST_RUN"/signals/conflict-healed "$GUEST_RUN"/signals/conflict-owner-done \
    "$GUEST_RUN/logs"/conflict-eio-write-owner.stderr \
    "$GUEST_RUN/logs"/conflict-reread-owner-1.stderr \
    "$GUEST_RUN/logs"/conflict-reread-owner-2.stderr
  guest_exec peer-n -- rm -f "$GUEST_RUN"/signals/conflict-rename-member-ready \
    "$GUEST_RUN"/signals/conflict-rename-member-done \
    "$GUEST_RUN"/signals/conflict-partitioned \
    "$GUEST_RUN"/signals/conflict-member-written \
    "$GUEST_RUN"/signals/conflict-healed "$GUEST_RUN"/signals/conflict-member-done \
    "$GUEST_RUN/logs"/conflict-eio-write-member.stderr \
    "$GUEST_RUN/logs"/conflict-reread-member-1.stderr \
    "$GUEST_RUN/logs"/conflict-reread-member-2.stderr
  # Legs run backgrounded with output to log files (not `run`): their
  # output is voluminous and the failure names the log, as in the
  # legacy phase.
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh conflict-member $GMD $GMC $RELAY_URL" \
    >"$RUN/logs/leg-conflict-member.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh conflict-owner $OD $OC $RELAY_URL" \
    >"$RUN/logs/leg-conflict-owner.out" 2>&1 &
  LEG_O=$!
  wait_guest_file peer-o 300 "$GUEST_RUN/signals/conflict-rename-owner-done" \
    "owner rename leg never finished (see logs/leg-conflict-owner.out)" "$LEG_O" \
    || return 1
  wait_guest_file peer-n 120 "$GUEST_RUN/signals/conflict-rename-member-done" \
    "member stale-handle probe never finished (see logs/leg-conflict-member.out)" "$LEG_N" \
    || return 1
  # Best-effort heal, called explicitly on every failure path below:
  # never an EXIT trap — a test-installed EXIT trap replaces bats'
  # own result reporting, and a failure inside the trap window then
  # vanishes with no verdict (the suite keeps going and the plan
  # undercounts). heal_relay itself never fails the test.
  heal_relay() {
    if [[ -n "${GATE_PARTITION_BIN:-}" ]]; then
      "$GATE_PARTITION_BIN" relay up 2>/dev/null || true
    fi
  }
  host_partition relay down || { heal_relay; echo "  FAIL: host could not partition relay" >&2; return 1; }
  guest_exec peer-o -- touch "$GUEST_RUN/signals/conflict-partitioned"
  guest_exec peer-n -- touch "$GUEST_RUN/signals/conflict-partitioned"
  wait_guest_file peer-o 180 "$GUEST_RUN/signals/conflict-owner-written" \
    "owner never committed under partition (see logs/leg-conflict-owner.out)" "$LEG_O" \
    || { heal_relay; return 1; }
  wait_guest_file peer-n 180 "$GUEST_RUN/signals/conflict-member-written" \
    "member never committed under partition (see logs/leg-conflict-member.out)" "$LEG_N" \
    || { heal_relay; return 1; }
  host_partition relay up || { heal_relay; echo "  FAIL: host could not heal relay" >&2; return 1; }
  guest_exec peer-o -- touch "$GUEST_RUN/signals/conflict-healed"
  guest_exec peer-n -- touch "$GUEST_RUN/signals/conflict-healed"
  wait "$LEG_N" || die "conflict member leg failed (see logs/leg-conflict-member.out)"
  wait "$LEG_O" || die "conflict owner leg failed (see logs/leg-conflict-owner.out)"
}
