#!/usr/bin/env bats
# shellcheck disable=SC2034
# (GATE_GUEST_BIN and friends are read indirectly by on_o/on_n from
# tests/harness/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate/gate-phase-11.bats — gate-path phase 11: unknown parent.
#
# Ports legacy microVM phase 15: a fresh pair
# on the same topology (no third VM) — the owner publishes two
# chained snapshots and stops before the member fetches any body;
# with both manifests converged but neither body servable, the
# member's reads fail EIO bounded (never hang, never partial),
# then heal once the owner returns. Fresh drives because the
# phase-9 forty-snapshot history would make every bound a backlog
# lottery. Runs as the build user under tests/gate/gate-entry.sh, after
# phases 1-10 in the same bats invocation. All drive/cred paths
# are recomputed here (bats runs each @test in its own process, so
# no shell variable crosses phases).

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN GATE_PARTITION_BIN CORE_ONLY GUEST_ENV RELAY_URL; do
    [ -n "${!v+x}" ] || { echo "gate-phase-11: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/var/lib/wyrd-e2e/e2e-env.sh" ] || { echo "gate-phase-11: GUEST_ENV=$GUEST_ENV is not the guest env file" >&2; return 1; }
  load "$GATE_BATS_LIB/guest"
  load "$GATE_BATS_LIB/partition"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

@test "withheld bodies fail EIO bounded and heal" {
  mkdir -p "$RUN/logs"
  GUO="$GUEST_RUN/creds/uparent-o"
  GUOD="$GUEST_RUN/drives/uparent-o"
  GUN="$GUEST_RUN/creds/uparent-n"
  GUND="$GUEST_RUN/drives/uparent-n"
  mint_guest_creds peer-o "$GUO" "$GUOD"
  mint_guest_creds peer-n "$GUN" "$GUND"
  # Post-mortem parity with the legacy phase: persist each wyrd
  # invocation's captured output to $RUN/logs before asserting (bats
  # merges stderr into $output, so one .out per step).
  run guest_wyrd peer-o init --identity-file "$GUO/identity" --passphrase-file "$GUO/passphrase" \
    "$GUOD"
  printf '%s\n' "$output" >"$RUN/logs/init-uparent.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run guest_wyrd peer-n device --identity-file "$GUN/identity" --passphrase-file "$GUN/passphrase" \
    "$GUND" pairing-request "$GUEST_RUN/pairing-uparent.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-uparent.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-n "$GUEST_RUN/pairing-uparent.txt" "$RUN/pairing-uparent.txt" \
    || { echo "pairing-uparent pull failed"; false; }
  DEV_UPARENT="$(grep '^device ' "$RUN/pairing-uparent.txt" | cut -d' ' -f2)"
  KEY_UPARENT="$(grep '^encryption-key ' "$RUN/pairing-uparent.txt" | cut -d' ' -f2)"
  [[ "${#DEV_UPARENT}" == 64 && "${#KEY_UPARENT}" == 64 ]] || die "uparent-n pairing material malformed"
  run guest_wyrd peer-o member --identity-file "$GUO/identity" \
    --passphrase-file "$GUO/passphrase" "$GUOD" \
    invite "$DEV_UPARENT" "$KEY_UPARENT" "$GUEST_RUN/invitation-uparent"
  printf '%s\n' "$output" >"$RUN/logs/invite-uparent.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-o "$GUEST_RUN/invitation-uparent" "$RUN/invitation-uparent" \
    || { echo "invitation-uparent pull failed"; false; }
  guest_push peer-n "$RUN/invitation-uparent" "$GUEST_RUN/invitation-uparent" \
    || { echo "invitation-uparent push failed"; false; }
  run guest_wyrd peer-n device --identity-file "$GUN/identity" --passphrase-file "$GUN/passphrase" \
    "$GUND" join "$GUEST_RUN/invitation-uparent"
  printf '%s\n' "$output" >"$RUN/logs/join-uparent.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_exec peer-o rm -f "$GUEST_RUN"/signals/uparent-owner-gone \
    "$GUEST_RUN"/signals/uparent-owner-back \
    "$GUEST_RUN"/signals/uparent-announced "$GUEST_RUN"/up-2.err "$GUEST_RUN"/up-1.err \
    "$GUEST_RUN"/up-2.got "$GUEST_RUN"/up-1.got "$GUEST_RUN"/up-9.err "$GUEST_RUN"/up-9.got
  guest_exec peer-n rm -f "$GUEST_RUN"/signals/uparent-member-probed \
    "$GUEST_RUN"/signals/uparent-member-healed \
    "$GUEST_RUN"/signals/uparent-announced "$GUEST_RUN"/up-2.err "$GUEST_RUN"/up-1.err \
    "$GUEST_RUN"/up-2.got "$GUEST_RUN"/up-1.got "$GUEST_RUN"/up-9.err "$GUEST_RUN"/up-9.got
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
