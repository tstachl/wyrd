#!/usr/bin/env bats
# shellcheck disable=SC2034
# (SSH/SSH_OPTS are read indirectly by on_o/on_n from
# tests/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate-phase-04.bats — gate-path phase 4: serving restart.
#
# Ports tests/alpha-microvm.sh phase 4 (microvm 8): route update
# rewires fetch across hosts (restart legs), then the upgrade-rejoin
# leg (owner upgrades its serving binary in-guest via `wyrd
# update`, member converges on the post-upgrade set with no remount
# and byte-identical views). Runs as the build user under
# tests/gate-entry.sh, after phases 1-3 in the same bats
# invocation. GMD/GMC/OD/OC/MD are recomputed here (bats runs each
# @test in its own process, so no shell variable crosses phases).

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN PEER_O PEER_N RELAY_URL WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY GATE_BATS_LIB CORE_ONLY GUEST_ENV; do
    [ -n "${!v+x}" ] || { echo "gate-phase-04: $v is not set (run through tests/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/mnt/wyrd-state/e2e-env.sh" ] || { echo "gate-phase-04: GUEST_ENV=$GUEST_ENV is not the guest-visible env file" >&2; return 1; }
  source "$BATS_TEST_DIRNAME/e2e-helpers.bash"
  e2e_require_wrapped "$STATE_DIR/e2e-env.sh" "./tests/gate-entry.sh (gate task)" --check-owner-pid
  SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes"
  SSH="ssh $SSH_OPTS"
}

@test "serving restart and upgrade rejoin rewire fetch across hosts" {
  mkdir -p "$RUN/logs"
  MD="$RUN/drives/member-n"
  OD="$GUEST_RUN/drives/owner"
  OC="$GUEST_RUN/creds/owner"
  GMD="$GUEST_RUN/drives/member-n"
  GMC="$GUEST_RUN/creds/member-n"
  rm -f "$RUN/member-ready" "$RUN/member-done"
  # Legs run backgrounded with output to log files (not `run`): their
  # output is voluminous and the failure names the log, as in the
  # legacy phase.
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh restart-member $GMD $GMC $RELAY_URL" \
    >"$RUN/logs/leg-restart-member.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh restarted-owner $OD $OC $RELAY_URL" \
    >"$RUN/logs/leg-restart-owner.out" 2>&1 &
  LEG_O=$!
  wait "$LEG_N" || { echo "restart member leg failed (see logs/leg-restart-member.out)"; false; }
  wait "$LEG_O" || { echo "restarted owner leg failed (see logs/leg-restart-owner.out)"; false; }
  # Snapshot the member's dedupe log size: phase 5 asserts it grew
  # across this restart (redelivery observed) and holds no duplicates.
  # Missing file means no acks yet — record 0 rather than failing here;
  # the leg fails on it if the log is still empty at the end.
  wc -l < "$MD/mailbox.seen" > "$RUN/seen-after-restart" 2>/dev/null \
    || echo 0 > "$RUN/seen-after-restart"
  # Upgrade rejoin (post-upgrade convergence leg,
  # nostr:nevent1qqsr9arqyq4vjvcvtfsqrqf3r5h0wp0zju2zrf7h42uzggl950e0h8qpz9mhxue69uhkwunpwdczuap49eehg57t5zx):
  # the owner upgrades its serving binary in-guest via `wyrd update`,
  # then re-announces; the member converges on the post-upgrade set
  # with no remount and byte-identical views. Same-checkout archive
  # first, crafted here per run (never checked in) from the
  # just-built binary under the canonical name, so resolve runs for
  # real; the true cross-version extension reuses these assertions
  # with a v0.2.0 binary (fixture-half design on the issue).
  rm -f "$RUN/member-u-ready" "$RUN/member-u-done" "$RUN/owner-u-updated" "$RUN/upgrade-blob.sha256"
  UPGRADE_ROOT="$(dirname "$BATS_TEST_DIRNAME")"
  UPGRADE_VERSION="$(grep '^version = ' "$UPGRADE_ROOT/Cargo.toml" | head -n 1 | cut -d'"' -f2)" \
    || { echo "workspace version unreadable ($UPGRADE_ROOT/Cargo.toml)"; false; }
  # `run --separate-stderr`: arch comes from stdout only — plain
  # `run` merges ssh's stderr (the UserKnownHostsFile=/dev/null
  # host-key warning) into $output and would poison the case match.
  run --separate-stderr on_o "uname -m"
  [ "$status" -eq 0 ] || { echo "$output"; echo "$stderr" >&2; false; }
  UPGRADE_GARCH="$(printf '%s' "$output" | tr -d ' \r\n')"
  case "$UPGRADE_GARCH" in
    x86_64) UPGRADE_ARCH="x86_64" ;;
    aarch64) UPGRADE_ARCH="aarch64" ;;
    *) echo "guest arch '$UPGRADE_GARCH' has no canonical archive name"; false ;;
  esac
  UPGRADE_NAME="wyrd-${UPGRADE_VERSION}-linux-${UPGRADE_ARCH}.tar.gz"
  UPGRADE_STAGE="$RUN/upgrade-archive/stage"
  rm -rf "$RUN/upgrade-archive"
  mkdir -p "$UPGRADE_STAGE/top/bin"
  cp "$WYRD_BIN" "$UPGRADE_STAGE/top/bin/wyrd" \
    || { echo "upgrade archive staging failed"; false; }
  tar -czf "$RUN/upgrade-archive/$UPGRADE_NAME" -C "$UPGRADE_STAGE" top \
    || { echo "upgrade archive packing failed"; false; }
  rm -rf "$UPGRADE_STAGE"
  hand_to_guest "$RUN/upgrade-archive"
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh upgrade-member $GMD $GMC $RELAY_URL" \
    >"$RUN/logs/leg-upgrade-member.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh upgrade-owner $OD $OC $RELAY_URL $GUEST_RUN/upgrade-archive/$UPGRADE_NAME" \
    >"$RUN/logs/leg-upgrade-owner.out" 2>&1 &
  LEG_O=$!
  wait "$LEG_N" || { echo "upgrade member leg failed (see logs/leg-upgrade-member.out)"; false; }
  wait "$LEG_O" || { echo "upgrade owner leg failed (see logs/leg-upgrade-owner.out)"; false; }
}
