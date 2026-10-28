#!/usr/bin/env bats
# shellcheck disable=SC2034
# (SSH/SSH_OPTS are read indirectly by on_o/on_n from
# tests/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate-phase-11.bats — gate-path phase 11: unknown parent.
#
# Ports legacy microVM phase 15: a fresh pair
# on the same topology (no third VM) — the owner publishes two
# chained snapshots and stops before the member fetches any body;
# with both manifests converged but neither body servable, the
# member's reads fail EIO bounded (never hang, never partial),
# then heal once the owner returns. Fresh drives because the
# phase-9 forty-snapshot history would make every bound a backlog
# lottery. Runs as the build user under tests/gate-entry.sh, after
# phases 1-10 in the same bats invocation. All drive/cred paths
# are recomputed here (bats runs each @test in its own process, so
# no shell variable crosses phases).

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN PEER_O PEER_N RELAY_URL WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY GATE_BATS_LIB GATE_PARTITION_BIN CORE_ONLY GUEST_ENV; do
    [ -n "${!v+x}" ] || { echo "gate-phase-11: $v is not set (run through tests/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/mnt/wyrd-state/run/e2e-env.sh" ] || { echo "gate-phase-11: GUEST_ENV=$GUEST_ENV is not the guest-visible env file" >&2; return 1; }
  source "$BATS_TEST_DIRNAME/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate-entry.sh (gate task)" --check-owner-pid
  SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes"
  SSH="ssh $SSH_OPTS"
}

@test "withheld bodies fail EIO bounded and heal" {
  mkdir -p "$RUN/logs"
  UO="$RUN/creds/uparent-o"
  UOD="$RUN/drives/uparent-o"
  UN="$RUN/creds/uparent-n"
  UND="$RUN/drives/uparent-n"
  GUO="$GUEST_RUN/creds/uparent-o"
  GUOD="$GUEST_RUN/drives/uparent-o"
  GUN="$GUEST_RUN/creds/uparent-n"
  GUND="$GUEST_RUN/drives/uparent-n"
  mkdir -p "$UO" "$UOD" "$UN" "$UND"
  for _cdir in "$UO" "$UN"; do
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$_cdir/identity"
    printf 'e2e-%s\n' "$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')" > "$_cdir/passphrase"
    chmod 600 "$_cdir/identity" "$_cdir/passphrase"
  done
  unset _cdir
  hand_to_guest "$UO" "$UOD" "$UN" "$UND"
  # Post-mortem parity with the legacy phase: persist each wyrd
  # invocation's captured output to $RUN/logs before asserting (bats
  # merges stderr into $output, so one .out per step).
  run as_guest "$WYRD_BIN" init --identity-file "$UO/identity" --passphrase-file "$UO/passphrase" \
    "$UOD"
  printf '%s\n' "$output" >"$RUN/logs/init-uparent.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run as_guest "$WYRD_BIN" device --identity-file "$UN/identity" --passphrase-file "$UN/passphrase" \
    "$UND" pairing-request "$RUN/pairing-uparent.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-uparent.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  DEV_UPARENT="$(grep '^device ' "$RUN/pairing-uparent.txt" | cut -d' ' -f2)"
  KEY_UPARENT="$(grep '^encryption-key ' "$RUN/pairing-uparent.txt" | cut -d' ' -f2)"
  [[ "${#DEV_UPARENT}" == 64 && "${#KEY_UPARENT}" == 64 ]] || die "uparent-n pairing material malformed"
  run as_guest "$WYRD_BIN" member --identity-file "$UO/identity" \
    --passphrase-file "$UO/passphrase" "$UOD" \
    invite "$DEV_UPARENT" "$KEY_UPARENT" "$RUN/invitation-uparent"
  printf '%s\n' "$output" >"$RUN/logs/invite-uparent.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run as_guest "$WYRD_BIN" device --identity-file "$UN/identity" --passphrase-file "$UN/passphrase" \
    "$UND" join "$RUN/invitation-uparent"
  printf '%s\n' "$output" >"$RUN/logs/join-uparent.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  rm -f "$RUN"/uparent-owner-gone "$RUN"/uparent-member-probed \
    "$RUN"/uparent-owner-back "$RUN"/uparent-member-healed \
    "$RUN"/uparent-announced "$RUN"/up-2.err "$RUN"/up-1.err \
    "$RUN"/up-2.got "$RUN"/up-1.got "$RUN"/up-9.err "$RUN"/up-9.got
  # Legs run backgrounded with output to log files (not `run`): their
  # output is voluminous and the failure names the log, as in the
  # legacy phase.
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh uparent-member $GUND $GUN $RELAY_URL" \
    >"$RUN/logs/leg-uparent-member.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh uparent-owner $GUOD $GUO $RELAY_URL" \
    >"$RUN/logs/leg-uparent-owner.out" 2>&1 &
  LEG_O=$!
  wait "$LEG_N" || die "unknown-parent member leg failed (see logs/leg-uparent-member.out)"
  wait "$LEG_O" || die "unknown-parent owner leg failed (see logs/leg-uparent-owner.out)"
}
