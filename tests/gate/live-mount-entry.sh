#!/usr/bin/env bash
# tests/gate/live-mount-entry.sh — entryPoint for
# [tasks.live-mount]. The host built the peer-o image from this
# revision (the image carries the wyrd-live-mount-tests libtest
# harness), provisioned and booted the guest, and waited for the agent.
# This entry reads the provision datum and validated manifest (never
# derives), holds the provision lock, publishes the run env, prepares
# the guest's log tree, and execs bats on the live-mount leg.
#
# Same contract as the other gate entries: the guest is reached only
# through GATE_GUEST_BIN (mandatory, unset refuses), direct elevated
# transport calls here because the unit runs as the build user. No ssh,
# no shared filesystem, no relay role: a mount evidence leg, not a
# convergence run.
#
# Usage: run by the gate task unit (always with --fresh), never by hand.
set -euo pipefail

if [[ "${1:-}" == "--fresh" ]]; then
  shift
  [[ $# -eq 0 ]] || { echo "usage: live-mount-entry.sh --fresh" >&2; exit 2; }
else
  echo "usage: live-mount-entry.sh --fresh" >&2; exit 2
fi
command -v bats >/dev/null || { echo "live-mount-entry.sh: bats is not on PATH" >&2; exit 1; }
command -v flock >/dev/null || { echo "live-mount-entry.sh: flock is not on PATH" >&2; exit 1; }
command -v jq >/dev/null || { echo "live-mount-entry.sh: jq is not on PATH" >&2; exit 1; }
command -v timeout >/dev/null || { echo "live-mount-entry.sh: timeout is not on PATH" >&2; exit 1; }

PROVISION_DIR="${GATE_TASK_PROVISION_DIR:?live-mount-entry.sh: GATE_TASK_PROVISION_DIR is not set}"
[ -d "$PROVISION_DIR" ] || { echo "live-mount-entry.sh: provision dir is missing" >&2; exit 1; }
GUEST_BIN="${GATE_GUEST_BIN:?live-mount-entry.sh: GATE_GUEST_BIN is not set (host must publish the guest transport)}"
[ -x "$GUEST_BIN" ] || { echo "live-mount-entry.sh: guest transport is not executable: $GUEST_BIN" >&2; exit 1; }
BATS_LIB="${GATE_BATS_LIB:?live-mount-entry.sh: GATE_BATS_LIB is not set}"
[ -d "$BATS_LIB" ] || { echo "live-mount-entry.sh: bats helpers are missing at $BATS_LIB" >&2; exit 1; }
entry_sudo_bin() {
  if [ -x /run/wrappers/bin/sudo ]; then printf '%s' /run/wrappers/bin/sudo; else printf '%s' sudo; fi # sudo_bin fallback
}
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
REV="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo "staged-tree (rev in task run record)")"
echo "live-mount-entry.sh: revision under test: $REV"

policy() { # <name>
  local file="$PROVISION_DIR/$1" value=""
  [ -f "$file" ] || { echo "live-mount-entry.sh: provision datum is missing: $1" >&2; return 1; }
  value="$(cat "$file")"
  [ -n "$value" ] || { echo "live-mount-entry.sh: provision datum is empty: $1" >&2; return 1; }
  printf '%s' "$value"
}

STATE_DIR="$(policy state-dir)" || exit 1
LOCK_FILE="$(policy lock-file)" || exit 1
RUN="$STATE_DIR/run"
[ -d "$RUN" ] || { echo "live-mount-entry.sh: run tree is missing at $RUN (host provisions it)" >&2; exit 1; }

# The peer-o address comes from the validated image manifest, never a
# derived subnet. The agent itself addresses the guest by role; the
# IP is the run record and the fail-fast proof the image built.
MANIFESTS_DIR="$PROVISION_DIR/manifests"
[ -d "$MANIFESTS_DIR" ] || { echo "live-mount-entry.sh: provision manifests are missing at $MANIFESTS_DIR" >&2; exit 1; }
PEER_IP=""
for m in "$MANIFESTS_DIR"/*.json; do
  [ -f "$m" ] || { echo "live-mount-entry.sh: no image manifests in $MANIFESTS_DIR" >&2; exit 1; }
  role="$(jq -r '.role // empty' "$m" 2>/dev/null || true)"
  ip="$(jq -r '.ip // empty' "$m" 2>/dev/null || true)"
  [[ "$role" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]] || { echo "live-mount-entry.sh: manifest role out of bounds in $m" >&2; exit 1; }
  [[ "$ip" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}/24$ ]] || { echo "live-mount-entry.sh: manifest ip out of bounds in $m: $ip" >&2; exit 1; }
  ip="${ip%/*}"
  _o1="${ip%%.*}"; _rest="${ip#*.}"; _o2="${_rest%%.*}"; _rest="${_rest#*.}"; _o3="${_rest%%.*}"; _o4="${ip##*.}"
  for _o in "$_o1" "$_o2" "$_o3" "$_o4"; do
    [ "$_o" -le 255 ] || { echo "live-mount-entry.sh: manifest ip octet out of bounds in $m: $ip" >&2; exit 1; }
  done
  unset _o1 _o2 _o3 _o4 _o _rest
  if [ "$role" = "peer-o" ]; then PEER_IP="$ip"; fi
done
[ -n "$PEER_IP" ] || { echo "live-mount-entry.sh: manifests name no peer-o role" >&2; exit 1; }

# Stable in-image paths: the peer-o image carries both the production
# CLI and the libtest harness, so no runner handoff resolution exists.
WYRD_BIN="/run/current-system/sw/bin/wyrd"
TEST_BIN="/run/current-system/sw/bin/wyrd-live-mount-tests"
E2E_ROOT="/var/lib/wyrd-e2e-live-mount"

# Hold the provision lock before writing any run-scoped state. The
# launcher holds it until the unit starts, so the first acquire retries.
exec 9>"$LOCK_FILE"
acquired=""
for _ in $(seq 1 60); do
  if flock -n 9 2>/dev/null; then acquired=1; break; fi
  sleep 1
done
[[ -n "$acquired" ]] || { echo "live-mount-entry.sh: another run holds $LOCK_FILE" >&2; exit 2; }
unset _

TOKEN="$(head -c16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
{
  echo "# Generated by tests/gate/live-mount-entry.sh; parsed by the wrapper guard, never sourced in-guest."
  printf 'WYRD_E2E_TOKEN=%s\n' "$TOKEN"
  printf 'WYRD_E2E_PID=%s\n' "$$"
  printf 'WYRD_PEER_O_IP=%q\n' "$PEER_IP"
  printf 'WYRD_BIN=%q\n' "$WYRD_BIN"
  printf 'LIVE_MOUNT_TEST_BIN=%q\n' "$TEST_BIN"
  printf 'GUEST_RUN=%q\n' "$E2E_ROOT"
} > "$RUN/e2e-env.sh"
chmod 0644 "$RUN/e2e-env.sh"

# Fresh guest state tree, owned by the suite user. The libtest writes
# its own TempDirs; the log tree is where the leg redirects stdout and
# stderr so the host's guest_log_dir fetch has the evidence.
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec peer-o -- rm -rf "$E2E_ROOT" \
  || { echo "live-mount-entry.sh: guest peer-o is unreachable (agent exec failed)" >&2; exit 1; }
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec peer-o -- mkdir -p "$E2E_ROOT/logs" \
  || { echo "live-mount-entry.sh: guest state reset failed" >&2; exit 1; }
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec peer-o -- chown -R e2e:users "$E2E_ROOT" \
  || { echo "live-mount-entry.sh: guest state handover failed" >&2; exit 1; }
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" push peer-o "$E2E_ROOT/e2e-env.sh" <"$RUN/e2e-env.sh" \
  || { echo "live-mount-entry.sh: env push to peer-o failed" >&2; exit 1; }
timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec peer-o -- chown e2e:users "$E2E_ROOT/e2e-env.sh" \
  || { echo "live-mount-entry.sh: env handover failed" >&2; exit 1; }

export WYRD_E2E_TOKEN="$TOKEN" WYRD_E2E_LOCK="$LOCK_FILE"
export STATE_DIR WYRD_BIN GATE_BATS_LIB GATE_GUEST_BIN
export GUEST_ENV="$E2E_ROOT/e2e-env.sh"
export GUEST_RUN="$E2E_ROOT"
export LIVE_MOUNT_TEST_BIN="$TEST_BIN"
export PEER_O_IP="$PEER_IP"

exec bats "$ROOT/tests/gate/live-mount.bats"
