#!/usr/bin/env bats
# shellcheck disable=SC2034
# (GATE_GUEST_BIN and friends are read indirectly by on_o/on_n from
# tests/harness/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate/gate-phase-04.bats — gate-path phase 4: serving restart.
#
# Ports legacy microVM phase 8: route update
# rewires fetch across hosts (restart legs), then the upgrade-rejoin
# leg (owner upgrades its serving binary in-guest via `wyrd
# update`, member converges on the post-upgrade set with no remount
# and byte-identical views). Runs as the build user under
# tests/gate/gate-entry.sh, after phases 1-3 in the same bats
# invocation. GMD/GMC/OD/OC/MD are recomputed here (bats runs each
# @test in its own process, so no shell variable crosses phases).

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN GATE_PARTITION_BIN CORE_ONLY GUEST_ENV RELAY_URL; do
    [ -n "${!v+x}" ] || { echo "gate-phase-04: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/var/lib/wyrd-e2e/e2e-env.sh" ] || { echo "gate-phase-04: GUEST_ENV=$GUEST_ENV is not the guest env file" >&2; return 1; }
  load "$GATE_BATS_LIB/guest"
  load "$GATE_BATS_LIB/partition"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

@test "serving restart and upgrade rejoin rewire fetch across hosts" {
  mkdir -p "$RUN/logs"
  GMD="$GUEST_RUN/drives/member-n"
  GMC="$GUEST_RUN/creds/member-n"
  guest_exec peer-n rm -f "$GUEST_RUN/signals/member-ready" "$GUEST_RUN/signals/member-done"
  OD="$GUEST_RUN/drives/owner"
  OC="$GUEST_RUN/creds/owner"
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
  guest_exec peer-n cat "$GMD/mailbox.seen" 2>/dev/null | wc -l | tr -d ' ' > "$RUN/seen-after-restart" \
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
  guest_exec peer-n rm -f "$GUEST_RUN/signals/member-u-ready" "$GUEST_RUN/signals/member-u-done"
  guest_exec peer-o rm -f "$GUEST_RUN/signals/owner-u-updated" "$GUEST_RUN/signals/upgrade-blob.sha256"
  UPGRADE_ROOT="$(dirname "$(dirname "$BATS_TEST_DIRNAME")")"
  UPGRADE_VERSION="$(grep '^version = ' "$UPGRADE_ROOT/Cargo.toml" | head -n 1 | cut -d'"' -f2)"
  [[ -n "$UPGRADE_VERSION" ]] \
    || { echo "workspace version unreadable ($UPGRADE_ROOT/Cargo.toml)"; false; }
  run guest_exec peer-o uname -m
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  UPGRADE_GARCH="$(printf '%s' "$output" | tr -d ' \r\n')"
  case "$UPGRADE_GARCH" in
    x86_64) UPGRADE_ARCH="x86_64" ;;
    aarch64) UPGRADE_ARCH="aarch64" ;;
    *) echo "guest arch '$UPGRADE_GARCH' has no canonical archive name"; false ;;
  esac
  UPGRADE_NAME="wyrd-${UPGRADE_VERSION}-linux-${UPGRADE_ARCH}.tar.gz"
  # The archive is crafted in-guest from the in-image binary: there is
  # no host copy of the binary anymore (no shared store), and the leg
  # consumes the archive at its guest path.
  guest_exec peer-o rm -rf "$GUEST_RUN/upgrade-archive" \
    || { echo "upgrade archive reset failed"; false; }
  guest_exec peer-o mkdir -p "$GUEST_RUN/upgrade-archive/stage/top/bin" \
    || { echo "upgrade archive staging failed"; false; }
  guest_exec peer-o cp "$WYRD_BIN" "$GUEST_RUN/upgrade-archive/stage/top/bin/wyrd" \
    || { echo "upgrade archive staging failed"; false; }
  guest_exec peer-o tar -czf "$GUEST_RUN/upgrade-archive/$UPGRADE_NAME" -C "$GUEST_RUN/upgrade-archive/stage" top \
    || { echo "upgrade archive packing failed"; false; }
  guest_exec peer-o rm -rf "$GUEST_RUN/upgrade-archive/stage" \
    || { echo "upgrade archive staging cleanup failed"; false; }
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh upgrade-member $GMD $GMC $RELAY_URL" \
    >"$RUN/logs/leg-upgrade-member.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh upgrade-owner $OD $OC $RELAY_URL $GUEST_RUN/upgrade-archive/$UPGRADE_NAME" \
    >"$RUN/logs/leg-upgrade-owner.out" 2>&1 &
  LEG_O=$!
  wait "$LEG_N" || { echo "upgrade member leg failed (see logs/leg-upgrade-member.out)"; false; }
  wait "$LEG_O" || { echo "upgrade owner leg failed (see logs/leg-upgrade-owner.out)"; false; }
}
