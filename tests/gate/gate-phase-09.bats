#!/usr/bin/env bats
# shellcheck disable=SC2034
# (GATE_GUEST_BIN and friends are read indirectly by on_o/on_n from
# tests/harness/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate/gate-phase-09.bats — gate-path phase 9: crash injection.
#
# Ports the legacy crash-injection leg: two SIGKILL legs
# against a fresh drive pair (the phase-6 pair is conflicted —
# writes fail EIO there by design — so crashing on it would prove
# nothing about the commit path). 13a kills the member with a
# pending obligation under a host-driven relay partition (role
# surgery, same shape as phase 6); 13b kills the owner mid-burst
# after 40 acknowledged writes. The member-first order is
# load-bearing (see the ORDER MATTERS note in the legacy phase).
# Every crash targets the shipped binary's real process death, and
# each leg records the fence its crash represents. Runs as the
# build user under tests/gate/gate-entry.sh, after phases 1-8 in the
# same bats invocation. All drive/cred paths are recomputed here
# (bats runs each @test in its own process, so no shell variable
# crosses phases).

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN GATE_PARTITION_BIN CORE_ONLY GUEST_ENV RELAY_URL; do
    [ -n "${!v+x}" ] || { echo "gate-phase-09: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/var/lib/wyrd-e2e/e2e-env.sh" ] || { echo "gate-phase-09: GUEST_ENV=$GUEST_ENV is not the guest env file" >&2; return 1; }
  load "$GATE_BATS_LIB/guest"
  load "$GATE_BATS_LIB/partition"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

@test "member and owner KILLs replay obligations and converge" {
  mkdir -p "$RUN/logs"
  GCO="$GUEST_RUN/creds/crash-o"
  GCOD="$GUEST_RUN/drives/crash-o"
  GCN="$GUEST_RUN/creds/crash-n"
  GCND="$GUEST_RUN/drives/crash-n"
  mint_guest_creds peer-o "$GCO" "$GCOD"
  mint_guest_creds peer-n "$GCN" "$GCND"
  # Post-mortem parity with the legacy phase: persist each wyrd
  # invocation's captured output to $RUN/logs before asserting (bats
  # merges stderr into $output, so one .out per step).
  run guest_wyrd peer-o init --identity-file "$GCO/identity" --passphrase-file "$GCO/passphrase" \
    "$GCOD"
  printf '%s\n' "$output" >"$RUN/logs/init-crash.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run guest_wyrd peer-n device --identity-file "$GCN/identity" --passphrase-file "$GCN/passphrase" \
    "$GCND" pairing-request "$GUEST_RUN/pairing-crash.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-crash.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-n "$GUEST_RUN/pairing-crash.txt" "$RUN/pairing-crash.txt" \
    || { echo "pairing-crash pull failed"; false; }
  DEV_CRASH="$(grep '^device ' "$RUN/pairing-crash.txt" | cut -d' ' -f2)"
  KEY_CRASH="$(grep '^encryption-key ' "$RUN/pairing-crash.txt" | cut -d' ' -f2)"
  [[ "${#DEV_CRASH}" == 64 && "${#KEY_CRASH}" == 64 ]] || die "crash-n pairing material malformed"
  run guest_wyrd peer-o member --identity-file "$GCO/identity" \
    --passphrase-file "$GCO/passphrase" "$GCOD" \
    invite "$DEV_CRASH" "$KEY_CRASH" "$GUEST_RUN/invitation-crash"
  printf '%s\n' "$output" >"$RUN/logs/invite-crash.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-o "$GUEST_RUN/invitation-crash" "$RUN/invitation-crash" \
    || { echo "invitation-crash pull failed"; false; }
  guest_push_e2e peer-n "$RUN/invitation-crash" "$GUEST_RUN/invitation-crash" \
    || { echo "invitation-crash push failed"; false; }
  run guest_wyrd peer-n device --identity-file "$GCN/identity" --passphrase-file "$GCN/passphrase" \
    "$GCND" join "$GUEST_RUN/invitation-crash"
  printf '%s\n' "$output" >"$RUN/logs/join-crash.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  # 13a: member KILL with a pending obligation under partition. Host
  # drives the partition like phase 6 (same relay, same EXIT-trap
  # discipline): the member authors alone while down.
  # ORDER MATTERS: 13a runs BEFORE 13b (the owner burst) — see the
  # legacy phase for the head-closure cost argument.
  guest_exec peer-o rm -f "$GUEST_RUN"/signals/crash-m-listed
  guest_exec peer-n rm -f "$GUEST_RUN"/signals/crash-m-listed "$GUEST_RUN"/signals/crash-m-ready "$GUEST_RUN"/signals/crash-m-partitioned \
    "$GUEST_RUN"/signals/crash-m-killed "$GUEST_RUN"/signals/crash-m-healed "$GUEST_RUN"/signals/crash-m-done "$GUEST_RUN"/signals/crash-m-witness-done
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh steady-owner $GCOD $GCO $RELAY_URL" \
    >"$RUN/logs/leg-steady-owner.out" 2>&1 &
  LEG_O=$!
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh crash-member $GCND $GCN $RELAY_URL" \
    >"$RUN/logs/leg-crash-member.out" 2>&1 &
  LEG_N=$!
  wait_guest_file peer-n 600 "$GUEST_RUN/signals/crash-m-ready" \
    "member never localized its tree for the obligation (see logs/leg-crash-member.out)" "$LEG_N" \
    || return 1
  # Best-effort heal, called explicitly on every failure path below:
  # never an EXIT trap — a test-installed EXIT trap replaces bats'
  # own result reporting, and a failure inside the trap window then
  # vanishes with no verdict (the suite keeps going and the plan
  # undercounts). heal_relay itself never fails the test.
  heal_relay() {
    gate_partition relay up 2>/dev/null || true
  }
  host_partition relay down || { heal_relay; echo "  FAIL: host could not partition relay" >&2; return 1; }
  guest_exec peer-n touch "$GUEST_RUN/signals/crash-m-partitioned"
  wait_guest_file peer-n 180 "$GUEST_RUN/signals/crash-m-killed" \
    "member never died on KILL under partition (see logs/leg-crash-member.out)" "$LEG_N" \
    || { heal_relay; return 1; }
  host_partition relay up || { heal_relay; echo "  FAIL: host could not heal relay" >&2; return 1; }
  guest_exec peer-n touch "$GUEST_RUN/signals/crash-m-healed"
  wait "$LEG_N" || die "crash member leg failed (see logs/leg-crash-member.out)"
  wait "$LEG_O" || die "crash witness owner leg failed (see logs/leg-steady-owner.out)"
  # 13b: owner burst + KILL, witness holds convergence across the death.
  guest_exec peer-o rm -f "$GUEST_RUN"/signals/crash-o-steady-ready "$GUEST_RUN"/signals/crash-o-back "$GUEST_RUN"/signals/crash-o-member-done
  guest_exec peer-n rm -f "$GUEST_RUN"/signals/crash-o-steady-ready "$GUEST_RUN"/signals/crash-o-back "$GUEST_RUN"/signals/crash-o-member-done
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh steady-member $GCND $GCN $RELAY_URL" \
    >"$RUN/logs/leg-steady-member.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh crash-owner $GCOD $GCO $RELAY_URL" \
    >"$RUN/logs/leg-crash-owner.out" 2>&1 &
  LEG_O=$!
  wait "$LEG_N" || die "crash witness member leg failed (see logs/leg-steady-member.out)"
  wait "$LEG_O" || die "crash owner leg failed (see logs/leg-crash-owner.out)"
}
