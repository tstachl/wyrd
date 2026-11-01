#!/usr/bin/env bats
# tests/drive/drive-service.bats — system-level evidence for the
# declarative drive service on the provisioned drive guest: one vault
# and one FUSE-serving unit declared through services.wyrd.drives,
# asserted live over the agent.
#
# Runs as the build user under tests/drive/drive-service-entry.sh,
# which assembles every variable used below. Not runnable directly:
# without the entry the setup refuses (fail fast, not skip — a green
# run with skipped tests would pretend coverage). Odin-only: the
# drive box is a real provisioned Incus guest.
#
# Guest calls ride on_d (as the suite user over the agent). State
# changes and bounded queries alike go through it; the entry already
# proved the reboot with per-probe bounds, and every suite call
# returns promptly on a booted guest. Privileged names appear only
# on on_d lines — guest privilege, never host.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup_file() {
  # Wrapper and state first: the entry already validated datum,
  # lock, token, provisioned the guest, and rebooted it. This
  # re-checks the contract per file without touching the guest:
  # provisioning and the reboot live in the entry precisely so no
  # runner-version setup_file semantics can skip them, and so an
  # unwrapped direct bats invocation has no mutation path.
  for v in STATE_DIR WYRD_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN; do
    [ -n "${!v:-}" ] || { echo "drive-service: $v is not set (run through tests/drive/drive-service-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ -d "$RUN" ] || { echo "drive-service: run tree is missing at $RUN" >&2; return 1; }
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/drive/drive-service-entry.sh (gate task)" --check-owner-pid
}

teardown() {
  # Failure forensics land in the log while the guest is still up:
  # the unit journal plus the full show dump for every supervised
  # unit, so a red test names its mechanism without a second run.
  if [ -f "$RUN/FAILED" ]; then
    echo "--- forensics for failed test: $BATS_TEST_DESCRIPTION ---"
    run on_d 'systemctl --no-pager --lines=30 status wyrd-vault-demo wyrd-mount-demo e2e-lockholder; echo ===; journalctl --no-pager -u wyrd-vault-demo -u wyrd-mount-demo -u e2e-lockholder | tail -n 60'
    echo "$output"
    # Serving-namespace FUSE table: the serving process hides its
    # namespace from the host (PrivateTmp), so dump it from inside
    # the serving child when one exists.
    run on_d 'command -v nsenter; command -v pgrep'
    NSENTER_T="$(printf '%s\n' "$output" | sed -n '1p')"
    PGREP_T="$(printf '%s\n' "$output" | sed -n '2p')"
    if [ -n "$NSENTER_T" ] && [ -n "$PGREP_T" ]; then
      run on_d 'systemctl show -p MainPID --value wyrd-mount-demo'
      MNT_MAIN="$output"
      # shellcheck disable=SC2086
      run on_d "$PGREP_T -P $MNT_MAIN wyrd 2>/dev/null | head -n 1"
      if [ -n "$output" ]; then
        # shellcheck disable=SC2086
        run on_d "sudo $NSENTER_T -t $output -m cat /proc/self/mountinfo 2>/dev/null | grep -E ' mnt|fuse' || echo 'no mnt/fuse entries in serving namespace'"
        echo "$output"
      else
        echo 'no serving child to enter'
      fi
    else
      echo "drive-service: could not resolve the namespace probe binaries"
    fi
  fi
  return 0
}

setup() {
  rm -f "$STATE_DIR/run/FAILED" 2>/dev/null || true
  for v in STATE_DIR WYRD_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN; do
    [ -n "${!v:-}" ] || { echo "drive-service: $v is not set (run through tests/drive/drive-service-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  load "$GATE_BATS_LIB/guest"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/drive/drive-service-entry.sh (gate task)" --check-owner-pid
}

@test "vault and serving units are active after boot" {
  run on_d 'systemctl is-active wyrd-vault-demo wyrd-mount-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  # The serving side comes up after the unit reports active, so poll
  # instead of asserting once. The service runs with PrivateTmp,
  # which puts it in its own namespace: the FUSE serving side stays
  # invisible from the host, so probe from inside the serving
  # process's namespace via nsenter. Absolute paths: the login PATH
  # these names resolved under is not inherited. Polled, since serving
  # starts after the unit reports active.
  run on_d 'command -v nsenter; command -v ls'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  NSENTER="$(printf '%s\n' "$output" | sed -n '1p')"
  LSCMD="$(printf '%s\n' "$output" | sed -n '2p')"
  [ -n "$NSENTER" ] && [ -n "$LSCMD" ] || { echo "could not resolve nsenter+ls: $output"; touch "$RUN/FAILED"; false; }
  run on_d 'systemctl show -p MainPID --value wyrd-mount-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  MNT_MAIN="$output"
  # shellcheck disable=SC2086
  run on_d "pgrep -P $MNT_MAIN wyrd | head -n 1"
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ -n "$output" ] || { echo "no supervised wyrd child under wrapper pid $MNT_MAIN"; touch "$RUN/FAILED"; false; }
  MNT_CHILD="$output"
  # Serving proof in two halves: a FUSE entry at exactly the
  # configured mountpoint in the serving namespace (an unmounted
  # directory passes an ls but has no table entry), then a
  # filesystem operation through it. Polled, since serving starts
  # after the unit reports active.
  # shellcheck disable=SC2086
  run on_d "for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do sudo $NSENTER -t $MNT_CHILD -m grep -E ' /mnt/wyrd .*fuse' /proc/self/mountinfo >/dev/null 2>&1 && sudo $NSENTER -t $MNT_CHILD -m $LSCMD /mnt/wyrd >/dev/null 2>&1 && break || sleep 5; done; sudo $NSENTER -t $MNT_CHILD -m grep -E ' /mnt/wyrd .*fuse' /proc/self/mountinfo && sudo $NSENTER -t $MNT_CHILD -m $LSCMD /mnt/wyrd"
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
}

@test "vault comes back after SIGKILL of the supervised child" {
  run on_d 'systemctl show -p MainPID --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  BEFORE_PID="$output"
  # shellcheck disable=SC2086
  run on_d "pgrep -P $BEFORE_PID wyrd | head -n 1"
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ -n "$output" ] || { echo "no supervised wyrd child under wrapper pid $BEFORE_PID"; touch "$RUN/FAILED"; false; }
  CHILD="$output"
  # shellcheck disable=SC2086
  run on_d "sudo kill -KILL $CHILD"
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  sleep 12
  run on_d 'systemctl show -p MainPID --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ "$output" != "$BEFORE_PID" ] || { echo "wrapper pid unchanged after child SIGKILL ($BEFORE_PID)"; touch "$RUN/FAILED"; false; }
  run on_d 'systemctl is-active wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  run on_d 'systemctl show -p NRestarts --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ "$output" == "1" ] || { echo "expected exactly one restart, got $output"; touch "$RUN/FAILED"; false; }
}

@test "held drive fails once with 78 and no retry" {
  # Stop the vault first so the holder is the only possible
  # contender: verifying contention while the vault still runs proves
  # only that *someone* holds the lock. Order is the proof.
  run on_d 'sudo systemctl stop wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  # The holder is a transient unit, not a background pid: precise
  # release via systemctl stop, no pattern matching anywhere. The
  # unit's PATH is systemd's default (no util-linux, no coreutils),
  # so resolve every binary absolutely — a bare name fails the unit
  # instantly (exit 69) and the "held" drive is held by nothing.
  run on_d 'command -v flock; command -v sleep'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  FLOCK="$(printf '%s\n' "$output" | sed -n '1p')"
  SLEEPCMD="$(printf '%s\n' "$output" | sed -n '2p')"
  [ -n "$FLOCK" ] && [ -n "$SLEEPCMD" ] || { echo "could not resolve flock+sleep: $output"; touch "$RUN/FAILED"; false; }
  # shellcheck disable=SC2086
  run on_d "sudo systemd-run --unit=e2e-lockholder --property=Type=simple $FLOCK /var/lib/wyrd/demo/LOCK $SLEEPCMD 120"
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  # Verify the holder before asserting anything about the service: a
  # start that only queued the unit proves nothing, and an unheld
  # "held drive" would make the refusal assertions vacuous. The
  # vault is already stopped above, so the holder is the only
  # possible contender — a failed second-party probe proves the
  # holder itself holds the lock, not a third party.
  run on_d 'systemctl is-active e2e-lockholder'
  [ "$status" -eq 0 ] || { echo "holder unit is not active: $output"; touch "$RUN/FAILED"; false; }
  # Poll the contention itself rather than asserting once:
  # is-active fires when the process spawns, possibly before its
  # first flock lands. Only the acquired lock is load-bearing. A
  # refused non-blocking flock prints nothing; agent failures print
  # diagnostics, so empty output plus non-zero status is the
  # contention signal and anything else keeps polling.
  HELD=""
  for _ in $(seq 1 15); do
    # shellcheck disable=SC2086
    run on_d "sudo $FLOCK -n /var/lib/wyrd/demo/LOCK true" 2>/dev/null || true
    if [ "$status" -ne 0 ] && [ -z "$output" ]; then HELD=1; break; fi
    sleep 2
  done
  [[ -n "$HELD" ]] || { echo "second-party non-blocking flock kept succeeding (last: $output)"; touch "$RUN/FAILED"; false; }
  # Clear the start-limit accounting first: boot (no identity), the
  # setup restart, and the SIGKILL restart already spent the unit's
  # StartLimitBurst budget inside one 300s window, so a bare start
  # would be refused with start-limit-hit (stale ExecMainStatus from
  # the stop) instead of exercising the held-drive path. reset-failed
  # is the documented reset for exactly this.
  run on_d 'sudo systemctl reset-failed wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  run on_d 'sudo systemctl start wyrd-vault-demo' || true
  echo "start job output (non-zero expected for the 78 path): $output"
  sleep 12
  run on_d 'systemctl show -p ActiveState --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ "$output" == "failed" ] || { echo "held-drive unit is $output, expected failed"; touch "$RUN/FAILED"; false; }
  run on_d 'systemctl show -p ExecMainStatus --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ "$output" == "78" ] || { echo "held-drive exit is $output, expected 78"; touch "$RUN/FAILED"; false; }
  run on_d 'systemctl show -p NRestarts --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ "$output" == "0" ] || { echo "held drive retried $output times, expected no retry"; touch "$RUN/FAILED"; false; }
}

@test "vault recovers once the holder releases" {
  run on_d 'sudo systemctl stop e2e-lockholder'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  run on_d 'sudo systemctl reset-failed wyrd-vault-demo; sudo systemctl start wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  sleep 8
  run on_d 'systemctl is-active wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
}
