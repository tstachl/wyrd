#!/usr/bin/env bash
# tests/drive/drive-service-entry.sh — entryPoint for
# [tasks.wyrd-drive-e2e]: the host built the drive image from this
# revision, provisioned the drive guest, booted it, and waited. This
# entry reads the provision datum (never derives), holds the
# provision lock, publishes the run env, prepares the guest (secrets
# and drives), proves a controlled reboot, and execs bats on the
# drive-service suite. Same datum discipline as
# tests/gate/gate-entry.sh; a task with different roles needs its
# own entry, not a flag.
#
# The guest is reached only through GATE_GUEST_BIN (mandatory, unset
# refuses): direct elevated transport calls in this entry (the unit
# runs as the build user, so the caller elevates with -n). There is
# no ssh and no shared filesystem; the Incus agent carries control
# and files. No relay participates: supervision, not convergence,
# is under test, so no relay datum is read.
#
# Usage: run by the gate task unit (always with --fresh), never by hand.
set -euo pipefail

if [[ "${1:-}" == "--fresh" ]]; then
  shift
  [[ $# -eq 0 ]] || { echo "usage: drive-service-entry.sh --fresh" >&2; exit 2; }
else
  echo "usage: drive-service-entry.sh --fresh" >&2; exit 2
fi
command -v bats >/dev/null || { echo "drive-service-entry.sh: bats is not on PATH" >&2; exit 1; }
command -v flock >/dev/null || { echo "drive-service-entry.sh: flock is not on PATH" >&2; exit 1; }
command -v jq >/dev/null || { echo "drive-service-entry.sh: jq is not on PATH" >&2; exit 1; }
command -v timeout >/dev/null || { echo "drive-service-entry.sh: timeout is not on PATH" >&2; exit 1; }

PROVISION_DIR="${GATE_TASK_PROVISION_DIR:?drive-service-entry.sh: GATE_TASK_PROVISION_DIR is not set}"
[ -d "$PROVISION_DIR" ] || { echo "drive-service-entry.sh: provision dir is missing" >&2; exit 1; }
GUEST_BIN="${GATE_GUEST_BIN:?drive-service-entry.sh: GATE_GUEST_BIN is not set (host must publish the guest transport)}"
[ -x "$GUEST_BIN" ] || { echo "drive-service-entry.sh: guest transport is not executable: $GUEST_BIN" >&2; exit 1; }
BATS_LIB="${GATE_BATS_LIB:?drive-service-entry.sh: GATE_BATS_LIB is not set}"
[ -d "$BATS_LIB" ] || { echo "drive-service-entry.sh: bats helpers are missing at $BATS_LIB" >&2; exit 1; }
# Elevation for the host-owned GATE_GUEST_BIN transport: the unit runs
# as the build user while the daemon socket is root-only, so the
# caller elevates with -n (fail fast, never prompt). Mirrors the
# gate-owned bats helpers (setuid wrapper first, PATH fallback).
entry_sudo_bin() {
  if [ -x /run/wrappers/bin/sudo ]; then printf '%s' /run/wrappers/bin/sudo; else printf '%s' sudo; fi # sudo_bin fallback
}
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
REV="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo "staged-tree (rev in task run record)")"
echo "drive-service-entry.sh: revision under test: $REV"

policy() { # <name>
  local file="$PROVISION_DIR/$1" value=""
  [ -f "$file" ] || { echo "drive-service-entry.sh: provision datum is missing: $1" >&2; return 1; }
  value="$(cat "$file")"
  [ -n "$value" ] || { echo "drive-service-entry.sh: provision datum is empty: $1" >&2; return 1; }
  printf '%s' "$value"
}

STATE_DIR="$(policy state-dir)" || exit 1
LOCK_FILE="$(policy lock-file)" || exit 1
RUN="$STATE_DIR/run"
[ -d "$RUN" ] || { echo "drive-service-entry.sh: run tree is missing at $RUN (host provisions it)" >&2; exit 1; }

# Guest address from the validated image manifest the host published
# (one <build>.json per image build, role-addressed inside). The
# entry names no build and derives no subnet: the manifest is the
# single declaration of addressing. The agent itself addresses the
# guest by role; the IP below is the run record and the fail-fast
# proof the drive image was provisioned.
MANIFESTS_DIR="$PROVISION_DIR/manifests"
[ -d "$MANIFESTS_DIR" ] || { echo "drive-service-entry.sh: provision manifests are missing at $MANIFESTS_DIR" >&2; exit 1; }
DRIVE_IP=""
for m in "$MANIFESTS_DIR"/*.json; do
  [ -f "$m" ] || { echo "drive-service-entry.sh: no image manifests in $MANIFESTS_DIR" >&2; exit 1; }
  role="$(jq -r '.role // empty' "$m" 2>/dev/null || true)"
  ip="$(jq -r '.ip // empty' "$m" 2>/dev/null || true)"
  [[ "$role" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]] || { echo "drive-service-entry.sh: manifest role out of bounds in $m" >&2; exit 1; }
  [[ "$ip" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}/24$ ]] || { echo "drive-service-entry.sh: manifest ip out of bounds in $m: $ip" >&2; exit 1; }
  ip="${ip%/*}"
  _o1="${ip%%.*}"; _rest="${ip#*.}"; _o2="${_rest%%.*}"; _rest="${_rest#*.}"; _o3="${_rest%%.*}"; _o4="${ip##*.}"
  for _o in "$_o1" "$_o2" "$_o3" "$_o4"; do
    [ "$_o" -le 255 ] || { echo "drive-service-entry.sh: manifest ip octet out of bounds in $m: $ip" >&2; exit 1; }
  done
  unset _o1 _o2 _o3 _o4 _o _rest
  if [ "$role" = "drive" ]; then DRIVE_IP="$ip"; fi
done
[ -n "$DRIVE_IP" ] || { echo "drive-service-entry.sh: manifests name no drive role" >&2; exit 1; }

# In-image wyrd: the image carries the subject package, so no runner
# handoff resolution exists. Stable NixOS path, same both sides.
WYRD_BIN="/run/current-system/sw/bin/wyrd"
E2E_ROOT="/var/lib/wyrd-e2e"

# Hold the provision lock before writing any run-scoped state, like
# the gate entry. The acquire retries briefly: the launcher holds
# this same lock until the unit starts.
exec 9>"$LOCK_FILE"
acquired=""
for _ in $(seq 1 60); do
  if flock -n 9 2>/dev/null; then acquired=1; break; fi
  sleep 1
done
[[ -n "$acquired" ]] || { echo "drive-service-entry.sh: another run holds $LOCK_FILE" >&2; exit 2; }
unset _

# Run environment, parsed by the wrapper guard (token and pid).
# Minted after the lock: the token must belong to the run that
# holds it.
TOKEN="$(head -c16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
{
  echo "# Generated by tests/drive/drive-service-entry.sh; parsed by the wrapper guard, never sourced in-guest."
  printf 'DRIVE_ROLE=%q\n' "drive"
  printf 'DRIVE_IP=%q\n' "$DRIVE_IP"
  printf 'WYRD_BIN=%q\n' "$WYRD_BIN"
  printf 'WYRD_E2E_TOKEN=%s\n' "$TOKEN"
  printf 'WYRD_E2E_PID=%s\n' "$$"
} > "$RUN/e2e-env.sh"
chmod 0644 "$RUN/e2e-env.sh"

# Guest preparation, as root over the agent (setup file moves stay
# root; everything the suite asserts runs as e2e): fresh state tree
# owned by the suite user, then the secrets and drives the module
# supervises. A guest that never answers fails the run here, before
# the suite. Timeout wraps the transport binary directly, never a
# shell function.
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec drive -- rm -rf "$E2E_ROOT" \
  || { echo "drive-service-entry.sh: guest drive is unreachable (agent exec failed)" >&2; exit 1; }
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec drive -- mkdir -p "$E2E_ROOT/drives" "$E2E_ROOT/creds" "$E2E_ROOT/logs" "$E2E_ROOT/signals" \
  || { echo "drive-service-entry.sh: guest drive state reset failed" >&2; exit 1; }
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec drive -- chown -R e2e:users "$E2E_ROOT" \
  || { echo "drive-service-entry.sh: guest drive state handover failed" >&2; exit 1; }

# Provision test secrets and two fresh drives: harness work, not
# service config. The module supervises; the entry owns the secrets.
# This lives here (not in bats setup_file) so it runs under plain
# bash, and so an unwrapped direct bats invocation has no mutation
# path (the bats file contains no provisioning path at all).
# Secrets never touch the host: everything runs in-guest. The agent
# lands as root with a bare lookup path, so the shell payload runs
# under an explicit PATH (coreutils resolve from the system
# profile, same as the suite's as_e2e routing).
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec drive -- /usr/bin/env PATH=/run/current-system/sw/bin:/usr/bin:/bin bash -c '
  set -e
  mkdir -p /etc/wyrd /var/lib/wyrd/demo /var/lib/wyrd/laptop /mnt/wyrd
  head -c32 /dev/urandom > /etc/wyrd/identity
  printf e2e-test-passphrase > /etc/wyrd/passphrase
  chmod 600 /etc/wyrd/identity /etc/wyrd/passphrase
' || { echo "drive-service-entry.sh: secret provisioning failed" >&2; exit 1; }
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec drive -- "$WYRD_BIN" init /var/lib/wyrd/demo --identity-file /etc/wyrd/identity --passphrase-file /etc/wyrd/passphrase \
  || { echo "drive-service-entry.sh: demo drive init failed" >&2; exit 1; }
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec drive -- "$WYRD_BIN" init /var/lib/wyrd/laptop --identity-file /etc/wyrd/identity --passphrase-file /etc/wyrd/passphrase \
  || { echo "drive-service-entry.sh: laptop drive init failed" >&2; exit 1; }

# Controlled reboot with boot-id verification: the reboot proves the
# units activate at boot with secrets and drives already in place. A
# manual start here would mask a missing boot enablement. The agent
# dies with the guest, so going down is probed by failed exec and
# coming back by a changed boot id — a guest that never went down
# and one that never came back refuse with different messages.
OLD_BOOT="$(timeout 60 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec drive -- /usr/bin/env PATH=/run/current-system/sw/bin:/usr/bin:/bin cat /proc/sys/kernel/random/boot_id)" \
  || { echo "drive-service-entry.sh: cannot read guest boot id" >&2; exit 1; }
OLD_BOOT="$(printf '%s' "$OLD_BOOT" | tr -d ' \n')"
[ -n "$OLD_BOOT" ] || { echo "drive-service-entry.sh: guest boot id is empty" >&2; exit 1; }
timeout 60 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec drive -- /usr/bin/env PATH=/run/current-system/sw/bin:/usr/bin:/bin systemctl reboot || true
echo "drive-service-entry.sh: reboot requested, waiting for the guest to go down (up to 120s)"
DOWN=""
for _ in $(seq 1 24); do
  if ! timeout 20 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec drive -- /usr/bin/env PATH=/run/current-system/sw/bin:/usr/bin:/bin true 2>/dev/null; then DOWN=1; break; fi
  sleep 5
done
[[ -n "$DOWN" ]] || { echo "drive-service-entry.sh: guest never went down for reboot" >&2; exit 1; }
echo "drive-service-entry.sh: guest is down, waiting for it to come back with a new boot id (up to 300s)"
BOOTED=""
for _ in $(seq 1 60); do
  NEW_BOOT="$(timeout 60 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec drive -- /usr/bin/env PATH=/run/current-system/sw/bin:/usr/bin:/bin cat /proc/sys/kernel/random/boot_id 2>/dev/null || true)"
  NEW_BOOT="$(printf '%s' "$NEW_BOOT" | tr -d ' \n')"
  if [[ -n "$NEW_BOOT" && "$NEW_BOOT" != "$OLD_BOOT" ]]; then BOOTED=1; break; fi
  sleep 5
done
[[ -n "$BOOTED" ]] || { echo "drive-service-entry.sh: guest went down for reboot but never came back with a new boot id" >&2; exit 1; }
echo "drive-service-entry.sh: guest rebooted ($OLD_BOOT -> $NEW_BOOT)"
unset _ DOWN BOOTED OLD_BOOT NEW_BOOT

export WYRD_E2E_TOKEN="$TOKEN" WYRD_E2E_LOCK="$LOCK_FILE"
export STATE_DIR WYRD_BIN
export GATE_BATS_LIB GATE_GUEST_BIN

exec bats "$ROOT/tests/drive/drive-service.bats"
