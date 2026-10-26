#!/usr/bin/env bash
# tests/gate-entry.sh — gate-path wrapper for the wyrd e2e suite.
#
# The entry point the gate provisioned unit runs: as the build user,
# on the traversable build copy, with the GATE_* capability env. The
# dev-loop counterpart is nix/microvm/run-microvm.sh, which owns root
# provisioning (bridge, taps, builds, boot) on its own machine; here
# the host already provisioned, built, booted, and waited, so this
# wrapper only assembles the suite environment, mints the run token,
# holds the provision lock, and execs bats on the suite files.
#
# Contract (pinned by tests/test-gate-entry.sh):
# - Reads, never derives: provision datum files (state-dir, lock-file,
#   subnet, ssh-user, relay-role, relay-port, roles) and the runner
#   handoff at GATE_RUNNERS_ENV. A missing or malformed input refuses
#   here, before any suite phase runs.
# - Datum schema: subnet is a dotted /24 prefix (e.g. 10.0.7);
#   relay-port is 1-65535; ssh-user is a login name; roles is
#   `|`-separated `name tap octet ssh-flag runner` records with an
#   ssh-flag of `ssh` or `-`. Shape violations refuse like missing
#   files do.
# - Runner names are task convention: the Odin task's provision builds
#   must name the wyrd and nak builds `wyrd` and `nak`; the suite
#   resolves them to WYRD_BIN and NAK_BIN.
# - Holds the provision lock (non-blocking) for the suite's lifetime,
#   so the e2e guard's lock check and a later teardown agree on who
#   owns the run. Minting and locking mirror run-microvm.sh:221-230.
# - Accepts only --fresh (the unit always passes it); anything else
#   is a caller mistake, not a default.
#
# Usage: gate-entry.sh --fresh
set -euo pipefail

[[ "${1:-}" == "--fresh" && $# -eq 1 ]] || { echo "usage: gate-entry.sh --fresh" >&2; exit 2; }
command -v bats >/dev/null || { echo "gate-entry.sh: bats is not on PATH" >&2; exit 1; }
command -v flock >/dev/null || { echo "gate-entry.sh: flock is not on PATH" >&2; exit 1; }
command -v ssh >/dev/null || { echo "gate-entry.sh: ssh is not on PATH" >&2; exit 1; }
command -v timeout >/dev/null || { echo "gate-entry.sh: timeout is not on PATH" >&2; exit 1; }

PROVISION_DIR="${GATE_TASK_PROVISION_DIR:?gate-entry.sh: GATE_TASK_PROVISION_DIR is not set}"
BATS_LIB="${GATE_BATS_LIB:?gate-entry.sh: GATE_BATS_LIB is not set}"
[ -d "$BATS_LIB" ] || { echo "gate-entry.sh: bats helpers are missing at $BATS_LIB" >&2; exit 1; }
RUNNERS_ENV="${GATE_RUNNERS_ENV:?gate-entry.sh: GATE_RUNNERS_ENV is not set}"
[ -f "$RUNNERS_ENV" ] || { echo "gate-entry.sh: runner handoff is missing at $RUNNERS_ENV" >&2; exit 1; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# policy <name>: the single-line provision datum. Missing or empty
# refuses: the host publishes every datum the suite consumes.
policy() { # <name>
  local file="$PROVISION_DIR/$1" value=""
  [ -f "$file" ] || { echo "gate-entry.sh: provision datum is missing: $1" >&2; return 1; }
  value="$(cat "$file")"
  [ -n "$value" ] || { echo "gate-entry.sh: provision datum is empty: $1" >&2; return 1; }
  printf '%s' "$value"
}

STATE_DIR="$(policy state-dir)" || exit 1
LOCK_FILE="$(policy lock-file)" || exit 1
SUBNET="$(policy subnet)" || exit 1
SSH_USER="$(policy ssh-user)" || exit 1
RELAY_ROLE="$(policy relay-role)" || exit 1
RELAY_PORT="$(policy relay-port)" || exit 1
# Shape, not just presence: a malformed datum would otherwise build
# bad endpoints silently. The host validates these too when it
# publishes them; this is the entry's fail-fast refusal.
[[ "$SUBNET" =~ ^([0-9]{1,3}\.){2}[0-9]{1,3}$ ]] || { echo "gate-entry.sh: subnet out of bounds: $SUBNET (want dotted /24 prefix)" >&2; exit 1; }
IFS=. read -ra _octets <<<"$SUBNET"
for octet in "${_octets[@]}"; do
  [ "$octet" -le 255 ] || { echo "gate-entry.sh: subnet octet out of bounds: $SUBNET" >&2; exit 1; }
done
unset _octets
[[ "$SSH_USER" =~ ^[a-z_][a-z0-9_-]{0,31}$ ]] || { echo "gate-entry.sh: ssh user out of bounds: $SSH_USER" >&2; exit 1; }
[[ "$RELAY_PORT" =~ ^[0-9]{1,5}$ ]] && [ "$RELAY_PORT" -ge 1 ] && [ "$RELAY_PORT" -le 65535 ] \
  || { echo "gate-entry.sh: relay port out of bounds: $RELAY_PORT" >&2; exit 1; }
ROLES_FILE="$PROVISION_DIR/roles"
[ -f "$ROLES_FILE" ] || { echo "gate-entry.sh: provision datum is missing: roles" >&2; exit 1; }

SSH_KEY="$STATE_DIR/sshkey"
[ -f "$SSH_KEY" ] || { echo "gate-entry.sh: ssh key is missing at $SSH_KEY (host provisions it)" >&2; exit 1; }
RUN="$STATE_DIR/run"
[ -d "$RUN" ] || { echo "gate-entry.sh: run tree is missing at $RUN (host provisions it)" >&2; exit 1; }

# Peer and relay endpoints from the published roles (name tap octet
# ssh-flag runner, `|`-separated): every ssh role becomes a dialable
# peer, the relay role becomes the ws URL. This entry is wyrd-task
# specific and names peer-o/peer-n explicitly; a task with different
# roles needs its own entry, not a flag. Fields are read into an
# array, never word-split: role fields are data, never patterns.
PEER_O=""; PEER_N=""; RELAY_OCTET=""
while IFS= read -r record || [ -n "$record" ]; do
  read -ra fields <<<"$record"
  [ "${#fields[@]}" -eq 5 ] || { echo "gate-entry.sh: role out of bounds: $record" >&2; exit 1; }
  # Octets are host address parts like the subnet's: numeric, in range.
  # The host validates on publish; this is the entry's fail-fast.
  [[ "${fields[2]}" =~ ^[0-9]{1,3}$ ]] && [ "${fields[2]}" -ge 1 ] && [ "${fields[2]}" -le 254 ] \
    || { echo "gate-entry.sh: role octet out of bounds: $record" >&2; exit 1; }
  if [ "${fields[0]}" = "$RELAY_ROLE" ]; then RELAY_OCTET="${fields[2]}"; fi
  case "${fields[3]}" in ssh|-) ;; *) { echo "gate-entry.sh: ssh flag out of bounds: $record" >&2; exit 1; } ;; esac
  if [ "${fields[0]}" = "peer-o" ] && [ "${fields[3]}" = "ssh" ]; then PEER_O="$SSH_USER@$SUBNET.${fields[2]}"; fi
  if [ "${fields[0]}" = "peer-n" ] && [ "${fields[3]}" = "ssh" ]; then PEER_N="$SSH_USER@$SUBNET.${fields[2]}"; fi
done < <(tr '|' '\n' <"$ROLES_FILE")
[ -n "$PEER_O" ] || { echo "gate-entry.sh: roles name no ssh peer-o" >&2; exit 1; }
[ -n "$PEER_N" ] || { echo "gate-entry.sh: roles name no ssh peer-n" >&2; exit 1; }
[ -n "$RELAY_OCTET" ] || { echo "gate-entry.sh: roles name no $RELAY_ROLE" >&2; exit 1; }
RELAY_URL="ws://$SUBNET.$RELAY_OCTET:$RELAY_PORT"

# Runner handoff: name=storepath lines the host build wrote. The names
# are task convention (see header); unknown names are ignored, missing
# required names refuse.
WYRD_BIN=""; NAK_BIN=""
while IFS= read -r line || [ -n "$line" ]; do
  [ -n "$line" ] || continue
  case "$line" in
    wyrd=*) WYRD_BIN="${line#wyrd=}" ;;
    nak=*) NAK_BIN="${line#nak=}" ;;
  esac
done <"$RUNNERS_ENV"
[ -n "$WYRD_BIN" ] || { echo "gate-entry.sh: runner handoff names no wyrd build" >&2; exit 1; }
[ -n "$NAK_BIN" ] || { echo "gate-entry.sh: runner handoff names no nak build" >&2; exit 1; }
[ -x "$WYRD_BIN" ] || { echo "gate-entry.sh: wyrd runner is not executable: $WYRD_BIN" >&2; exit 1; }
[ -x "$NAK_BIN" ] || { echo "gate-entry.sh: nak runner is not executable: $NAK_BIN" >&2; exit 1; }

# Run token and owner pid, published where the e2e guard parses them
# (parsed, never sourced). Written into the build-owned run tree, not
# the root-owned state dir.
TOKEN="$(head -c16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
{
  echo "# Generated by tests/gate-entry.sh; the e2e guard parses it, never sources it."
  printf 'WYRD_E2E_TOKEN=%s\n' "$TOKEN"
  printf 'WYRD_E2E_PID=%s\n' "$$"
} > "$RUN/e2e-env.sh"
chmod 0644 "$RUN/e2e-env.sh"

# Hold the provision lock for the suite's lifetime (mirrors
# run-microvm.sh): a successful non-blocking acquire by anyone else
# would mean no wrapper is alive.
exec 9>"$LOCK_FILE"
flock -n 9 || { echo "gate-entry.sh: another run holds $LOCK_FILE" >&2; exit 2; }

export WYRD_E2E_TOKEN="$TOKEN" WYRD_E2E_LOCK="$LOCK_FILE"
export STATE_DIR SSH_KEY WYRD_BIN NAK_BIN PEER_O PEER_N RELAY_URL
export GATE_BATS_LIB
export E2E_INTERVAL="${E2E_INTERVAL:-}"

exec bats "$ROOT/tests/gate-entry-smoke.bats"
