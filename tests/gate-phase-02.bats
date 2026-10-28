#!/usr/bin/env bats
# shellcheck disable=SC2034
# (SSH/SSH_OPTS are read indirectly by on_o/on_n from
# tests/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate-phase-02.bats — gate-path phase 2: member-n invite/join split.
#
# Ports legacy microVM phase 6: mint member-n
# credential material on the host, hand it to the guest owner,
# verify both peers carry the baked suite and execute the shared
# binary, then pairing-request / invite / join / re-invite on the
# host. Runs as the build user under tests/gate-entry.sh, after
# phase 1 in the same bats invocation: the owner creds and drives
# this phase consumes ($RUN/creds/owner, $RUN/drives/owner) are
# created by phase 1's shared-core run on the shared state.
# Later phases follow this file's shape.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN PEER_O PEER_N WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY GATE_BATS_LIB CORE_ONLY GUEST_ENV; do
    [ -n "${!v+x}" ] || { echo "gate-phase-02: $v is not set (run through tests/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/mnt/wyrd-state/run/e2e-env.sh" ] || { echo "gate-phase-02: GUEST_ENV=$GUEST_ENV is not the guest-visible env file" >&2; return 1; }
  source "$BATS_TEST_DIRNAME/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate-entry.sh (gate task)" --check-owner-pid
  SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes"
  SSH="ssh $SSH_OPTS"
}

@test "member-n invite/join split completes" {
  # Created here, not at the top: phase 1's guest pre-clean empties
  # the shared state root, so anything made earlier would be wiped
  # before the first redirect that needs it.
  mkdir -p "$RUN/logs"
  MC="$RUN/creds/member-n"
  MD="$RUN/drives/member-n"
  mkdir -p "$MC" "$MD"
  # Credential bytes without python: od + tr are always present.
  # Created as the build user, then handed to the guest owner: the
  # guest legs open these, and cross-uid opens fail closed.
  head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$MC/identity"
  printf 'e2e-%s\n' "$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')" > "$MC/passphrase"
  chmod 600 "$MC/identity" "$MC/passphrase"
  hand_to_guest "$MC" "$MD"
  # Guests must carry the baked suite, the runner-written env, and a
  # working wyrd binary from the shared host store before anything else.
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
  run as_guest "$WYRD_BIN" device --identity-file "$MC/identity" --passphrase-file "$MC/passphrase" \
    "$MD" pairing-request "$RUN/pairing-n.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-n.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  DEV="$(grep '^device ' "$RUN/pairing-n.txt" | cut -d' ' -f2)"
  KEY="$(grep '^encryption-key ' "$RUN/pairing-n.txt" | cut -d' ' -f2)"
  [[ "${#DEV}" == 64 && "${#KEY}" == 64 ]] || { echo "member-n pairing material malformed"; false; }
  run as_guest "$WYRD_BIN" member --identity-file "$RUN/creds/owner/identity" \
    --passphrase-file "$RUN/creds/owner/passphrase" "$RUN/drives/owner" \
    invite "$DEV" "$KEY" "$RUN/invitation-n"
  printf '%s\n' "$output" >"$RUN/logs/invite-n.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run as_guest "$WYRD_BIN" device --identity-file "$MC/identity" --passphrase-file "$MC/passphrase" \
    "$MD" join "$RUN/invitation-n"
  printf '%s\n' "$output" >"$RUN/logs/join-n.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  # Rerun-resume cover (bound-admission-closure child 2): re-inviting
  # an admitted device resumes its catch-up instead of failing — same
  # device and key, fresh destination (the destination policy refuses
  # existing files, so reusing invitation-n would fail at the claim,
  # before the resume logic under test).
  run as_guest "$WYRD_BIN" member --identity-file "$RUN/creds/owner/identity" \
    --passphrase-file "$RUN/creds/owner/passphrase" "$RUN/drives/owner" \
    invite "$DEV" "$KEY" "$RUN/invitation-n-rerun"
  printf '%s\n' "$output" >"$RUN/logs/invite-n-rerun.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  [ -s "$RUN/invitation-n-rerun" ] || { echo "re-invitation file missing after rerun"; false; }
}
