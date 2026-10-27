#!/usr/bin/env bats
# shellcheck disable=SC2034
# (SSH/SSH_OPTS are read indirectly by on_o/on_n from
# tests/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate-phase-09.bats — gate-path phase 9: crash injection.
#
# Ports tests/alpha-microvm.sh phase 9 (microvm 13): two SIGKILL legs
# against a fresh drive pair (the phase-6 pair is conflicted —
# writes fail EIO there by design — so crashing on it would prove
# nothing about the commit path). 13a kills the member with a
# pending obligation under a host-driven relay partition (tap-r
# surgery, same shape as phase 6); 13b kills the owner mid-burst
# after 40 acknowledged writes. The member-first order is
# load-bearing (see the ORDER MATTERS note in the legacy phase).
# Every crash targets the shipped binary's real process death, and
# each leg records the fence its crash represents. Runs as the
# build user under tests/gate-entry.sh, after phases 1-8 in the
# same bats invocation. All drive/cred paths are recomputed here
# (bats runs each @test in its own process, so no shell variable
# crosses phases).

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN PEER_O PEER_N RELAY_URL WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY GATE_BATS_LIB GATE_PARTITION_BIN CORE_ONLY GUEST_ENV; do
    [ -n "${!v+x}" ] || { echo "gate-phase-09: $v is not set (run through tests/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/mnt/wyrd-state/run/e2e-env.sh" ] || { echo "gate-phase-09: GUEST_ENV=$GUEST_ENV is not the guest-visible env file" >&2; return 1; }
  source "$BATS_TEST_DIRNAME/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate-entry.sh (gate task)" --check-owner-pid
  SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes"
  SSH="ssh $SSH_OPTS"
}

@test "member and owner KILLs replay obligations and converge" {
  mkdir -p "$RUN/logs"
  CO="$RUN/creds/crash-o"
  COD="$RUN/drives/crash-o"
  CN="$RUN/creds/crash-n"
  CND="$RUN/drives/crash-n"
  GCO="$GUEST_RUN/creds/crash-o"
  GCOD="$GUEST_RUN/drives/crash-o"
  GCN="$GUEST_RUN/creds/crash-n"
  GCND="$GUEST_RUN/drives/crash-n"
  mkdir -p "$CO" "$COD" "$CN" "$CND"
  for _cdir in "$CO" "$CN"; do
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$_cdir/identity"
    printf 'e2e-%s\n' "$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')" > "$_cdir/passphrase"
    chmod 600 "$_cdir/identity" "$_cdir/passphrase"
  done
  unset _cdir
  hand_to_guest "$CO" "$COD" "$CN" "$CND"
  # Post-mortem parity with the legacy phase: persist each wyrd
  # invocation's captured output to $RUN/logs before asserting (bats
  # merges stderr into $output, so one .out per step).
  run as_guest "$WYRD_BIN" init --identity-file "$CO/identity" --passphrase-file "$CO/passphrase" \
    "$COD"
  printf '%s\n' "$output" >"$RUN/logs/init-crash.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run as_guest "$WYRD_BIN" device --identity-file "$CN/identity" --passphrase-file "$CN/passphrase" \
    "$CND" pairing-request "$RUN/pairing-crash.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-crash.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  DEV_CRASH="$(grep '^device ' "$RUN/pairing-crash.txt" | cut -d' ' -f2)"
  KEY_CRASH="$(grep '^encryption-key ' "$RUN/pairing-crash.txt" | cut -d' ' -f2)"
  [[ "${#DEV_CRASH}" == 64 && "${#KEY_CRASH}" == 64 ]] || die "crash-n pairing material malformed"
  run as_guest "$WYRD_BIN" member --identity-file "$CO/identity" \
    --passphrase-file "$CO/passphrase" "$COD" \
    invite "$DEV_CRASH" "$KEY_CRASH" "$RUN/invitation-crash"
  printf '%s\n' "$output" >"$RUN/logs/invite-crash.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run as_guest "$WYRD_BIN" device --identity-file "$CN/identity" --passphrase-file "$CN/passphrase" \
    "$CND" join "$RUN/invitation-crash"
  printf '%s\n' "$output" >"$RUN/logs/join-crash.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  # 13a: member KILL with a pending obligation under partition. Host
  # drives the partition like phase 6 (same tap, same EXIT-trap
  # discipline): the member authors alone while down.
  # ORDER MATTERS: 13a runs BEFORE 13b (the owner burst) — see the
  # legacy phase for the head-closure cost argument.
  rm -f "$RUN"/crash-m-listed "$RUN"/crash-m-ready "$RUN"/crash-m-partitioned "$RUN"/crash-m-killed \
    "$RUN"/crash-m-healed "$RUN"/crash-m-done "$RUN"/crash-m-witness-done
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh steady-owner $GCOD $GCO $RELAY_URL" \
    >"$RUN/logs/leg-steady-owner.out" 2>&1 &
  LEG_O=$!
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh crash-member $GCND $GCN $RELAY_URL" \
    >"$RUN/logs/leg-crash-member.out" 2>&1 &
  LEG_N=$!
  wait_conflict_file 600 "$RUN/crash-m-ready" \
    "member never localized its tree for the obligation (see logs/leg-crash-member.out)" "$LEG_N"
  # Best-effort heal: must never fail the test itself (it runs on
  # the EXIT path) and must never die — die exits the process and
  # cannot be caught by `|| true`. The capability was validated at
  # entry and setup, so reachability is guaranteed; guard directly
  # on the bin instead of routing through host_partition's die.
  heal_tap() {
    if [[ -n "${GATE_PARTITION_BIN:-}" ]]; then
      "$GATE_PARTITION_BIN" tap-r up 2>/dev/null || true
    fi
  }
  trap heal_tap EXIT
  host_partition tap-r down || die "host could not partition tap-r"
  touch "$RUN/crash-m-partitioned"
  wait_conflict_file 180 "$RUN/crash-m-killed" \
    "member never died on KILL under partition (see logs/leg-crash-member.out)" "$LEG_N"
  host_partition tap-r up || die "host could not heal tap-r"
  trap - EXIT
  touch "$RUN/crash-m-healed"
  wait "$LEG_N" || die "crash member leg failed (see logs/leg-crash-member.out)"
  wait "$LEG_O" || die "crash witness owner leg failed (see logs/leg-steady-owner.out)"
  # 13b: owner burst + KILL, witness holds convergence across the death.
  rm -f "$RUN"/crash-o-steady-ready "$RUN"/crash-o-back "$RUN"/crash-o-member-done
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh steady-member $GCND $GCN $RELAY_URL" \
    >"$RUN/logs/leg-steady-member.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh crash-owner $GCOD $GCO $RELAY_URL" \
    >"$RUN/logs/leg-crash-owner.out" 2>&1 &
  LEG_O=$!
  wait "$LEG_N" || die "crash witness member leg failed (see logs/leg-steady-member.out)"
  wait "$LEG_O" || die "crash owner leg failed (see logs/leg-crash-owner.out)"
}
