#!/usr/bin/env bats
# tests/drive/drive-service.bats — system-level evidence for the
# declarative drive service on the provisioned wyrd-drive-vm guest:
# one vault and one mount declared through services.wyrd.drives,
# asserted live over ssh.
#
# Runs as the build user under tests/drive/drive-service-entry.sh,
# which assembles every variable used below. Not runnable directly:
# without the entry the setup refuses (fail fast, not skip — a green
# run with skipped tests would pretend coverage). Odin-only: the
# drive box is a real provisioned microVM.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

# setup_file() provisions over ssh before setup() ever runs, so the
# ssh options live at file scope, not in setup(). The entry exports
# every variable used below; setup() still verifies them for the
# per-test path. LogLevel=ERROR silences the "Permanently added"
# warning ssh prints to stderr on first contact: bats merges stderr
# into $output, so the warning would corrupt exact-match assertions
# and pid captures below.
SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes -o LogLevel=ERROR"

setup_file() {
  # Provision test secrets and two fresh drives once per file:
  # harness work, not service config. The module supervises; the
  # suite owns the secrets.
  for v in SSH_KEY DRIVE_SSH; do
    [ -n "${!v:-}" ] || { echo "drive-service: $v is not set (run through tests/drive/drive-service-entry.sh)" >&2; return 1; }
  done
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'set -e
    sudo mkdir -p /etc/wyrd /var/lib/wyrd/demo /var/lib/wyrd/laptop /mnt/wyrd
    # sudo resolves the hostname on every invocation; the isolated
    # guest has no DNS, so pin it or every sudo below stalls on the
    # resolver and burns the ssh timeout instead of asserting.
    grep -q wyrd-drive /etc/hosts || echo "127.0.0.1 wyrd-drive" | sudo tee -a /etc/hosts >/dev/null
    head -c32 /dev/urandom | sudo tee /etc/wyrd/identity >/dev/null
    sudo chmod 600 /etc/wyrd/identity
    printf e2e-test-passphrase | sudo tee /etc/wyrd/passphrase >/dev/null
    sudo chmod 600 /etc/wyrd/passphrase
    sudo wyrd init /var/lib/wyrd/demo --identity-file /etc/wyrd/identity --passphrase-file /etc/wyrd/passphrase
    sudo wyrd init /var/lib/wyrd/laptop --identity-file /etc/wyrd/identity --passphrase-file /etc/wyrd/passphrase
    sudo systemctl restart wyrd-vault-demo wyrd-mount-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
}

teardown() {
  # Failure forensics land in the log while the guest is still up:
  # the unit journal plus the full show dump for every supervised
  # unit, so a red test names its mechanism without a second run.
  if [ -f "$RUN/FAILED" ]; then
    echo "--- forensics for failed test: $BATS_TEST_DESCRIPTION ---"
    # shellcheck disable=SC2086
    run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl --no-pager --lines=30 status wyrd-vault-demo wyrd-mount-demo e2e-lockholder; echo ===; journalctl --no-pager -u wyrd-vault-demo -u wyrd-mount-demo -u e2e-lockholder | tail -n 60'
    echo "$output"
  fi
  return 0
}

setup() {
  rm -f "$STATE_DIR/run/FAILED" 2>/dev/null || true
  for v in STATE_DIR DRIVE_SSH WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY; do
    [ -n "${!v:-}" ] || { echo "drive-service: $v is not set (run through tests/drive/drive-service-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/drive/drive-service-entry.sh (gate task)" --check-owner-pid
}

@test "vault and mount units are active after boot" {
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl is-active wyrd-vault-demo wyrd-mount-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  # The mount completes after the unit reports active, so poll
  # instead of asserting once. The service runs with PrivateTmp,
  # which puts it in its own mount namespace: the FUSE mount is
  # invisible from the host (host-side mountpoint -q says rc=32 and
  # host stat shows a plain subdir), so probe from inside the
  # serving process's namespace via nsenter. Absolute binary paths:
  # sudo's secure_path is not the login PATH. Polled, since serving
  # starts after the unit reports active.
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'command -v nsenter; command -v ls'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  NSENTER="$(printf '%s\n' "$output" | sed -n '1p')"
  LSCMD="$(printf '%s\n' "$output" | sed -n '2p')"
  [ -n "$NSENTER" ] && [ -n "$LSCMD" ] || { echo "could not resolve nsenter+ls: $output"; touch "$RUN/FAILED"; false; }
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl show -p MainPID --value wyrd-mount-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  MNT_MAIN="$output"
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" "pgrep -P $MNT_MAIN wyrd | head -n 1"
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ -n "$output" ] || { echo "no supervised wyrd child under mount wrapper pid $MNT_MAIN"; touch "$RUN/FAILED"; false; }
  MNT_CHILD="$output"
  # shellcheck disable=SC2086
  run timeout 180 ssh $SSH_OPTS "$DRIVE_SSH" "for i in 1 2 3 4 5 6 7 8 9 10 11 12; do sudo $NSENTER -t $MNT_CHILD -m $LSCMD /mnt/wyrd >/dev/null 2>&1 && break || sleep 5; done; sudo $NSENTER -t $MNT_CHILD -m $LSCMD /mnt/wyrd"
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
}

@test "vault comes back after SIGKILL of the supervised child" {
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl show -p MainPID --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  BEFORE_PID="$output"
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" "pgrep -P $BEFORE_PID wyrd | head -n 1"
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ -n "$output" ] || { echo "no supervised wyrd child under wrapper pid $BEFORE_PID"; touch "$RUN/FAILED"; false; }
  CHILD="$output"
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" "sudo kill -KILL $CHILD"
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  sleep 12
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl show -p MainPID --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ "$output" != "$BEFORE_PID" ] || { echo "wrapper pid unchanged after child SIGKILL ($BEFORE_PID)"; touch "$RUN/FAILED"; false; }
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl is-active wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl show -p NRestarts --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ "$output" == "1" ] || { echo "expected exactly one restart, got $output"; touch "$RUN/FAILED"; false; }
}

@test "held drive fails once with 78 and no retry" {
  # Stop the vault first so the holder is the only possible
  # contender: verifying contention while the vault still runs proves
  # only that *someone* holds the lock. Order is the proof.
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'sudo systemctl stop wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  # The holder is a transient unit, not a background pid: precise
  # release via systemctl stop, no pattern matching anywhere. The
  # unit's PATH is systemd's default (no util-linux, no coreutils),
  # so resolve every binary absolutely — a bare name fails the unit
  # instantly (exit 69) and the "held" drive is held by nothing.
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'command -v flock; command -v sleep'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  FLOCK="$(printf '%s\n' "$output" | sed -n '1p')"
  SLEEPCMD="$(printf '%s\n' "$output" | sed -n '2p')"
  [ -n "$FLOCK" ] && [ -n "$SLEEPCMD" ] || { echo "could not resolve flock+sleep: $output"; touch "$RUN/FAILED"; false; }
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" "sudo systemd-run --unit=e2e-lockholder --property=Type=simple $FLOCK /var/lib/wyrd/demo/LOCK $SLEEPCMD 120"
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  # Verify the holder before asserting anything about the service: a
  # start that only queued the unit proves nothing, and an unheld
  # "held drive" would make the refusal assertions vacuous. The
  # vault is already stopped above, so the holder is the only
  # possible contender — a failed second-party probe proves the
  # holder itself holds the lock, not a third party.
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl is-active e2e-lockholder'
  [ "$status" -eq 0 ] || { echo "holder unit is not active: $output"; touch "$RUN/FAILED"; false; }
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" "sudo $FLOCK -n /var/lib/wyrd/demo/LOCK true"
  [ "$status" -ne 0 ] || { echo "second-party non-blocking flock succeeded: nothing holds the drive"; touch "$RUN/FAILED"; false; }
  # Clear the start-limit accounting first: boot (no identity), the
  # setup restart, and the SIGKILL restart already spent the unit's
  # StartLimitBurst budget inside one 300s window, so a bare start
  # would be refused with start-limit-hit (stale ExecMainStatus from
  # the stop) instead of exercising the held-drive path. reset-failed
  # is the documented reset for exactly this.
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'sudo systemctl reset-failed wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'sudo systemctl start wyrd-vault-demo' || true
  echo "start job output (non-zero expected for the 78 path): $output"
  sleep 12
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl show -p ActiveState --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ "$output" == "failed" ] || { echo "held-drive unit is $output, expected failed"; touch "$RUN/FAILED"; false; }
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl show -p ExecMainStatus --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ "$output" == "78" ] || { echo "held-drive exit is $output, expected 78"; touch "$RUN/FAILED"; false; }
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl show -p NRestarts --value wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  [ "$output" == "0" ] || { echo "held drive retried $output times, expected no retry"; touch "$RUN/FAILED"; false; }
}

@test "vault recovers once the holder releases" {
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'sudo systemctl stop e2e-lockholder'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'sudo systemctl reset-failed wyrd-vault-demo; sudo systemctl start wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
  sleep 8
  # shellcheck disable=SC2086
  run timeout 120 ssh $SSH_OPTS "$DRIVE_SSH" 'systemctl is-active wyrd-vault-demo'
  [ "$status" -eq 0 ] || { echo "$output"; touch "$RUN/FAILED"; false; }
}
