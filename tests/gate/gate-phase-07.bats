#!/usr/bin/env bats
# shellcheck disable=SC2034
# (GATE_GUEST_BIN and friends are read indirectly by on_o/on_n from
# tests/harness/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate/gate-phase-07.bats — gate-path phase 7: offline reopen.
#
# Ports legacy microVM phase 11: both drives
# reopen offline after unmount, then in-guest exports prove
# flush-committed state (cross-host writes, wave-2 files, fetched
# cold objects) and conflict siblings survive the restart — pulled
# host-side for the asserts. Runs as
# the build user under tests/gate/gate-entry.sh, after phases 1-6 in the
# same bats invocation. All drive/cred paths are recomputed here (bats
# runs each @test in its own process, so no shell variable crosses
# phases).
# Depends on phase 5's delete (fetch-member removes stale-1.txt):
# the export fails closed on remote-only content, so without that
# delete this phase dies at the member export — coupled phases, as
# in the legacy flow.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN GATE_PARTITION_BIN CORE_ONLY GUEST_ENV RELAY_URL; do
    [ -n "${!v+x}" ] || { echo "gate-phase-07: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/var/lib/wyrd-e2e/e2e-env.sh" ] || { echo "gate-phase-07: GUEST_ENV=$GUEST_ENV is not the guest env file" >&2; return 1; }
  load "$GATE_BATS_LIB/guest"
  load "$GATE_BATS_LIB/partition"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

@test "flush-committed state and conflict siblings survive offline reopen" {
  mkdir -p "$RUN/logs"
  GMC="$GUEST_RUN/creds/member-n"
  GMD="$GUEST_RUN/drives/member-n"
  GOC="$GUEST_RUN/creds/owner"
  GOD="$GUEST_RUN/drives/owner"
  # Post-mortem parity with the legacy phase: persist each wyrd
  # invocation's captured output to $RUN/logs before asserting (bats
  # merges stderr into $output, so one .out per step).
  run guest_wyrd peer-n device --identity-file "$GMC/identity" --passphrase-file "$GMC/passphrase" \
    "$GMD" id
  printf '%s\n' "$output" >"$RUN/logs/reopen-n.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run guest_wyrd peer-o device --identity-file "$GOC/identity" \
    --passphrase-file "$GOC/passphrase" "$GOD" id
  printf '%s\n' "$output" >"$RUN/logs/reopen-o.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  # Durability is the point of the reopen, not the `id` call: export
  # both drives in-guest, pull the trees, and assert the cross-host
  # writes survived the restart. (Cleared first: the guest export
  # refuses populated destinations, and a non-fresh rerun would leave
  # the previous run's trees behind.)
  guest_exec peer-o rm -rf "$GUEST_RUN/export-owner"
  guest_exec peer-n rm -rf "$GUEST_RUN/export-member"
  rm -rf "$RUN/export-owner" "$RUN/export-member"
  run guest_wyrd peer-o export \
    --identity-file "$GOC/identity" \
    --passphrase-file "$GOC/passphrase" \
    "$GOD" "$GUEST_RUN/export-owner"
  printf '%s\n' "$output" >"$RUN/logs/export-o.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-o "$GUEST_RUN/export-owner" "$RUN/export-owner" \
    || { echo "owner export pull failed"; false; }
  run guest_wyrd peer-n export \
    --identity-file "$GMC/identity" --passphrase-file "$GMC/passphrase" \
    "$GMD" "$GUEST_RUN/export-member"
  printf '%s\n' "$output" >"$RUN/logs/export-m.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-n "$GUEST_RUN/export-member" "$RUN/export-member" \
    || { echo "member export pull failed"; false; }
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
