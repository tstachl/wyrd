#!/usr/bin/env bash
# tests/microvm/drive-service-leg.sh — system-level evidence for the
# declarative drive service on a real NixOS guest. Boots the
# wyrd-drive-vm configuration under KVM, provisions test secrets plus
# two fresh drives over ssh, and asserts the supervisor contract live:
# both units active after boot, the vault back after SIGKILL of the
# supervised child, and exactly one exit-78 with no retry against a
# held drive.
#
# Runs on the KVM host (odin) as root: /dev/kvm, the host-global run
# lock, and an ssh client are required. Never in CI. Coordinate the
# shared host before launching (one run at a time); the log carries
# the tested revision so a green run is evidence for exactly one tip.
#
# Usage (from the checkout): sudo ./tests/microvm/drive-service-leg.sh [--port N] [--keep]
#   --port N  host-forwarded guest ssh port (default 2222)
#   --keep    retain the VM state dir for forensics instead of wiping it
set -euo pipefail

PORT=2222
KEEP=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --port) PORT="$2"; shift 2;;
    --keep) KEEP=1; shift;;
    *) echo "usage: drive-service-leg.sh [--port N] [--keep]" >&2; exit 2;;
  esac
done

[[ "$(id -u)" == "0" ]] || { echo "FAIL: run as root (needs /dev/kvm and the host run lock)" >&2; exit 3; }
command -v nix >/dev/null || { echo "FAIL: nix is not on PATH" >&2; exit 3; }
command -v ssh >/dev/null || { echo "FAIL: ssh is not on PATH" >&2; exit 3; }
[[ -e /dev/kvm ]] || { echo "FAIL: /dev/kvm is missing on this host" >&2; exit 3; }
if command -v sshpass >/dev/null; then SSHPASS=sshpass; else SSHPASS="nix run --impure nixpkgs#sshpass --"; fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
REV="$(git -C "$ROOT" rev-parse HEAD)"
SHORT="$(git -C "$ROOT" rev-parse --short HEAD)"
LOG="$ROOT/drive-service-vm-$SHORT.log"
: > "$LOG"
exec > >(tee -a "$LOG") 2>&1

say() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
die() { say "FAIL: $*"; exit 1; }

say "revision under test: $REV"
say "log: $LOG"

# Host-global run lock: one shared-infra run at a time. Non-blocking:
# a held lock means another agent owns the host, so refuse loudly
# instead of queueing behind work this leg cannot see.
exec 9>/var/lib/wyrd-microvm/lock
flock -n 9 || die "host run lock is held; coordinate with the lock holder and retry"
say "host lock acquired"

STATE="$ROOT/.drive-service-vm-state-$SHORT"
mkdir -p "$STATE"
cleanup() {
  say "teardown: stopping guest"
  $SSHPASS -p 'wyrd-e2e' ssh -p "$PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 root@localhost poweroff 2>/dev/null || true
  # Kill by number, never by pattern over a channel whose argv could
  # match: the runner first, then any qemu child still holding the
  # state dir (pgrep excludes itself; the parent script's argv holds
  # no state-dir string, so the remainder cannot be us).
  kill "$QEMU_PID" 2>/dev/null || true
  sleep 2
  for qp in $(pgrep -f "qemu.*$SHORT" 2>/dev/null || true); do
    [[ "$qp" == "$$" ]] || kill "$qp" 2>/dev/null || true
  done
  if [[ "$KEEP" == "0" ]]; then rm -rf "$STATE"; else say "kept state dir: $STATE"; fi
  say "teardown done"
}
trap cleanup EXIT

say "building the guest VM image"
VM_RUNNER="$(nix build --print-out-paths --no-link "$ROOT#nixosConfigurations.wyrd-drive-vm.config.system.build.vm" | tail -n 1)"
shopt -s nullglob
RUNNERS=("$VM_RUNNER"/bin/run-*-vm)
shopt -u nullglob
[[ "${#RUNNERS[@]}" == "1" && -x "${RUNNERS[0]}" ]] || die "no single run-vm script under $VM_RUNNER"
RUNNER_BIN="${RUNNERS[0]}"
say "runner: $RUNNER_BIN"

say "booting the guest (user-net ssh on port $PORT)"
cd "$STATE"
QEMU_NET_OPTS="hostfwd=tcp::$PORT-:22" "$RUNNER_BIN" -nographic >"$STATE/qemu.log" 2>&1 &
QEMU_PID=$!
cd "$ROOT"

sshx() { $SSHPASS -p 'wyrd-e2e' ssh -p "$PORT" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 root@localhost "$@"; }

say "waiting for guest ssh (up to 300s)"
for _ in $(seq 1 60); do sshx true 2>/dev/null && break; sleep 5; done
sshx true || die "guest ssh never came up; see $STATE/qemu.log"

# Provision test secrets and two fresh drives: harness work, not
# service config. The module supervises; the leg owns the secrets.
say "provisioning identity, passphrase, and both drives"
sshx 'set -e
  mkdir -p /etc/wyrd /var/lib/wyrd/demo /var/lib/wyrd/laptop /mnt/wyrd
  head -c32 /dev/urandom > /etc/wyrd/identity; chmod 600 /etc/wyrd/identity
  printf e2e-test-passphrase > /etc/wyrd/passphrase; chmod 600 /etc/wyrd/passphrase
  wyrd init /var/lib/wyrd/demo --identity-file /etc/wyrd/identity --passphrase-file /etc/wyrd/passphrase
  wyrd init /var/lib/wyrd/laptop --identity-file /etc/wyrd/identity --passphrase-file /etc/wyrd/passphrase
  systemctl restart wyrd-vault-demo wyrd-mount-demo' || die "provisioning failed"

say "assert: both units active after boot"
sshx 'systemctl is-active wyrd-vault-demo wyrd-mount-demo' || die "units not active after boot"
sshx 'mountpoint -q /mnt/wyrd && ls /mnt/wyrd' || die "mountpoint not serving"
say "PASS: vault and mount active, mountpoint serving"

# SIGKILL of the supervised child (not the wrapper): the wrapper maps
# signal death to the restart-worthy status and systemd restarts.
say "assert: vault comes back after SIGKILL of the child"
BEFORE_PID="$(sshx 'systemctl show -p MainPID --value wyrd-vault-demo')"
CHILD="$(sshx "pgrep -P $BEFORE_PID wyrd" | head -n 1)"
[[ -n "$CHILD" ]] || die "no supervised wyrd child under wrapper pid $BEFORE_PID"
sshx "kill -KILL $CHILD"
sleep 12
AFTER_PID="$(sshx 'systemctl show -p MainPID --value wyrd-vault-demo')"
[[ "$AFTER_PID" != "$BEFORE_PID" ]] || die "wrapper pid unchanged after child SIGKILL ($BEFORE_PID)"
sshx 'systemctl is-active wyrd-vault-demo' || die "vault not active after child SIGKILL"
RESTARTS="$(sshx 'systemctl show -p NRestarts --value wyrd-vault-demo')"
[[ "$RESTARTS" == "1" ]] || die "expected exactly one restart, got $RESTARTS"
say "PASS: SIGKILL restarted once (pid $BEFORE_PID -> $AFTER_PID)"

# Held drive: the kernel flock on <drive>/LOCK makes the service's
# open fail with StoreLocked, the CLI exits 78, and the unit's
# RestartPreventExitStatus refuses the retry. Exactly once.
say "assert: held drive fails once with 78 and no retry"
sshx 'flock /var/lib/wyrd/demo/LOCK sleep 120 >/dev/null 2>&1 &
      echo $! > /tmp/lockholder.pid'
sleep 1
sshx 'systemctl restart wyrd-vault-demo' || true
sleep 12
STATE_OUT="$(sshx 'systemctl show -p ActiveState --value wyrd-vault-demo')"
[[ "$STATE_OUT" == "failed" ]] || die "held-drive unit is $STATE_OUT, expected failed"
STATUS78="$(sshx 'systemctl show -p ExecMainStatus --value wyrd-vault-demo')"
[[ "$STATUS78" == "78" ]] || die "held-drive exit is $STATUS78, expected 78"
RESTARTS78="$(sshx 'systemctl show -p NRestarts --value wyrd-vault-demo')"
[[ "$RESTARTS78" == "0" ]] || die "held drive retried $RESTARTS78 times, expected no retry"
say "PASS: held drive failed once with 78, no retry"
sshx 'kill "$(cat /tmp/lockholder.pid)" 2>/dev/null || true; rm -f /tmp/lockholder.pid'
sshx 'systemctl reset-failed wyrd-vault-demo; systemctl start wyrd-vault-demo'
sleep 8
sshx 'systemctl is-active wyrd-vault-demo' || die "vault did not recover after the holder released"
say "PASS: vault recovers once the holder releases"

say "drive-service-vm: ALL CHECKS PASSED at $REV"
