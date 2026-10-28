#!/usr/bin/env bats
# shellcheck disable=SC2034
# (SSH/SSH_OPTS are read indirectly by on_o/on_n from
# tests/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate-phase-07.bats — gate-path phase 7: offline reopen.
#
# Ports legacy microVM phase 11: both drives
# reopen offline after unmount, then host-side exports prove
# flush-committed state (cross-host writes, wave-2 files, fetched
# cold objects) and conflict siblings survive the restart. Runs as
# the build user under tests/gate-entry.sh, after phases 1-6 in the
# same bats invocation. MC/MD are recomputed here (bats runs each
# @test in its own process, so no shell variable crosses phases).
# Depends on phase 5's delete (fetch-member removes stale-1.txt):
# the export fails closed on remote-only content, so without that
# delete this phase dies at the member export — coupled phases, as
# in the legacy flow.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN PEER_O PEER_N RELAY_URL WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY GATE_BATS_LIB GATE_PARTITION_BIN CORE_ONLY GUEST_ENV; do
    [ -n "${!v+x}" ] || { echo "gate-phase-07: $v is not set (run through tests/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/mnt/wyrd-state/run/e2e-env.sh" ] || { echo "gate-phase-07: GUEST_ENV=$GUEST_ENV is not the guest-visible env file" >&2; return 1; }
  source "$BATS_TEST_DIRNAME/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate-entry.sh (gate task)" --check-owner-pid
  SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes"
  SSH="ssh $SSH_OPTS"
}

@test "flush-committed state and conflict siblings survive offline reopen" {
  mkdir -p "$RUN/logs"
  MC="$RUN/creds/member-n"
  MD="$RUN/drives/member-n"
  # Post-mortem parity with the legacy phase: persist each wyrd
  # invocation's captured output to $RUN/logs before asserting (bats
  # merges stderr into $output, so one .out per step).
  run as_guest "$WYRD_BIN" device --identity-file "$MC/identity" --passphrase-file "$MC/passphrase" \
    "$MD" id
  printf '%s\n' "$output" >"$RUN/logs/reopen-n.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run as_guest "$WYRD_BIN" device --identity-file "$RUN/creds/owner/identity" \
    --passphrase-file "$RUN/creds/owner/passphrase" "$RUN/drives/owner" id
  printf '%s\n' "$output" >"$RUN/logs/reopen-o.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  # Durability is the point of the reopen, not the `id` call: export
  # both drives and assert the cross-host writes survived the restart.
  # (Cleared first: export refuses populated destinations, and a
  # non-fresh rerun would leave the previous run's trees behind.)
  rm -rf "$RUN/export-owner" "$RUN/export-member"
  run as_guest "$WYRD_BIN" export \
    --identity-file "$RUN/creds/owner/identity" \
    --passphrase-file "$RUN/creds/owner/passphrase" \
    "$RUN/drives/owner" "$RUN/export-owner"
  printf '%s\n' "$output" >"$RUN/logs/export-o.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run as_guest "$WYRD_BIN" export \
    --identity-file "$MC/identity" --passphrase-file "$MC/passphrase" \
    "$MD" "$RUN/export-member"
  printf '%s\n' "$output" >"$RUN/logs/export-m.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  [[ "$(cat "$RUN/export-owner/after-restart.txt")" == "after-restart" ]] \
    || die "owner export lost the post-restart write"
  # Wave 2 (phase 7 in legacy numbering) overwrote shared.txt with
  # owner-write-2 and the host diffed identical live heads: the
  # surviving cross-host write on this path is the overwrite, not
  # the wave-1 original.
  [[ "$(cat "$RUN/export-member/shared.txt")" == "owner-write-2" ]] \
    || die "member export lost the cross-host write"
  # Wave 2's created files survive the restart too, not just the
  # overwrite: same export, same durability proof, one line each.
  [[ "$(cat "$RUN/export-member/wave2-a.txt")" == "wave2-a-bytes" ]] \
    || die "member export lost wave2-a.txt"
  [[ "$(cat "$RUN/export-member/wave2-dir/wave2-b.txt")" == "wave2-b-bytes" ]] \
    || die "member export lost wave2-dir/wave2-b.txt"
  # The fetch phase pulled cold-2.txt over the network; the export
  # proves the verified remote object survives process death.
  [[ "$(cat "$RUN/export-member/cold-2.txt")" == "cold-bytes" ]] \
    || die "member export lost the fetched cold-2.txt"
  # Conflict siblings: the conflict legs localized both versions on
  # both drives (cat of @1/@2 in-leg), so the offline export emits
  # both takes as name@N siblings. Numbering is SnapshotId byte
  # order — pinned nowhere, so compare the set, per drive.
  for side in owner member; do
    for v in 1 2; do
      [[ -f "$RUN/export-$side/conflict-c.txt@$v" ]] \
        || die "$side export lost conflict-c.txt@$v"
    done
    got="$(printf '%s\n' "$(cat "$RUN/export-$side/conflict-c.txt@1")" "$(cat "$RUN/export-$side/conflict-c.txt@2")" | sort)"
    want="$(printf '%s\n' "owner-conflict-1" "member-conflict-1" | sort)"
    [[ "$got" == "$want" ]] \
      || die "$side export sibling set mismatch: [$got]"
  done
}
