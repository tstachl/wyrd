#!/usr/bin/env bats
# shellcheck disable=SC2034
# (GATE_GUEST_BIN and friends are read indirectly by on_o/on_n from
# tests/harness/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate/gate-phase-02.bats — gate-path phase 2: member-n invite/join split.
#
# Ports the legacy member-n invite/join split: mint member-n
# credential material in-guest, verify both peers carry the baked
# suite and execute the in-image binaries, then pairing-request /
# invite / join / re-invite with the public material ferried
# host-side. Runs as the build user under tests/gate/gate-entry.sh,
# after phase 1 in the same bats invocation: the owner creds and
# drives this phase consumes live in the owner peer from phase 1's
# shared-core run. Later phases follow this file's shape.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN GATE_PARTITION_BIN CORE_ONLY GUEST_ENV RELAY_URL; do
    [ -n "${!v+x}" ] || { echo "gate-phase-02: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/var/lib/wyrd-e2e/e2e-env.sh" ] || { echo "gate-phase-02: GUEST_ENV=$GUEST_ENV is not the guest env file" >&2; return 1; }
  load "$GATE_BATS_LIB/guest"
  load "$GATE_BATS_LIB/partition"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

@test "member-n invite/join split completes" {
  # Created here, not at the top: phase 1's guest pre-clean empties
  # the state roots, so anything made earlier would be wiped before
  # the first redirect that needs it.
  mkdir -p "$RUN/logs"
  # Guest paths (the creds and drives live in the guests; the hosts
  # only ferries the public pairing/invitation material between them).
  GMC="$GUEST_RUN/creds/member-n"
  GMD="$GUEST_RUN/drives/member-n"
  GOC="$GUEST_RUN/creds/owner"
  GOD="$GUEST_RUN/drives/owner"
  mint_guest_creds peer-n "$GMC" "$GMD"
  # Guests must carry the baked suite, the pushed env, and working
  # in-image binaries before anything else.
  run on_n "test -f $GUEST_TESTS/alpha-common.sh && test -f $GUEST_ENV"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run on_n "source $GUEST_ENV; test -x \"\$WYRD_BIN\""
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run on_o "source $GUEST_ENV; test -x \"\$WYRD_BIN\""
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  # Post-mortem parity with the legacy phase: persist each wyrd
  # invocation's captured output to $RUN/logs before asserting, so a
  # failure leaves the same log files behind (bats merges stderr into
  # $output, so there is one .out per step, no .stderr split).
  run guest_wyrd peer-n device --identity-file "$GMC/identity" --passphrase-file "$GMC/passphrase" \
    "$GMD" pairing-request "$GUEST_RUN/pairing-n.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-n.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-n "$GUEST_RUN/pairing-n.txt" "$RUN/pairing-n.txt" \
    || { echo "pairing pull failed"; false; }
  DEV="$(grep '^device ' "$RUN/pairing-n.txt" | cut -d' ' -f2)"
  KEY="$(grep '^encryption-key ' "$RUN/pairing-n.txt" | cut -d' ' -f2)"
  [[ "${#DEV}" == 64 && "${#KEY}" == 64 ]] || { echo "member-n pairing material malformed"; false; }
  run guest_wyrd peer-o member --identity-file "$GOC/identity" \
    --passphrase-file "$GOC/passphrase" "$GOD" \
    invite "$DEV" "$KEY" "$GUEST_RUN/invitation-n"
  printf '%s\n' "$output" >"$RUN/logs/invite-n.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-o "$GUEST_RUN/invitation-n" "$RUN/invitation-n" \
    || { echo "invitation pull failed"; false; }
  guest_push peer-n "$RUN/invitation-n" "$GUEST_RUN/invitation-n" \
    || { echo "invitation push failed"; false; }
  run guest_wyrd peer-n device --identity-file "$GMC/identity" --passphrase-file "$GMC/passphrase" \
    "$GMD" join "$GUEST_RUN/invitation-n"
  printf '%s\n' "$output" >"$RUN/logs/join-n.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  # Rerun-resume cover (bound-admission-closure child 2): re-inviting
  # an admitted device resumes its catch-up instead of failing — same
  # device and key, fresh destination (the destination policy refuses
  # existing files, so reusing invitation-n would fail at the claim,
  # before the resume logic under test).
  run guest_wyrd peer-o member --identity-file "$GOC/identity" \
    --passphrase-file "$GOC/passphrase" "$GOD" \
    invite "$DEV" "$KEY" "$GUEST_RUN/invitation-n-rerun"
  printf '%s\n' "$output" >"$RUN/logs/invite-n-rerun.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-o "$GUEST_RUN/invitation-n-rerun" "$RUN/invitation-n-rerun" \
    || { echo "re-invitation pull failed"; false; }
  [ -s "$RUN/invitation-n-rerun" ] || { echo "re-invitation file missing after rerun"; false; }
}
