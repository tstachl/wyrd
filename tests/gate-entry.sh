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
# - The published run env file carries dev-loop key parity
#   (WYRD_BIN, RELAY_*, CHECKOUT, E2E_ROOT plus token, owner pid and
#   debug passthroughs): the guest legs source it, so a key the dev
#   loop publishes must be present here too.
# - Holds the provision lock for the suite's lifetime, so the e2e
#   guard's lock check and a later teardown agree on who owns the
#   run. The initial acquire retries briefly: the launcher releases
#   its own hold only as the unit starts, so a fast unit can reach
#   this acquire while the launcher is still exiting. Minting and
#   locking mirror run-microvm.sh:221-230.
# - Accepts only --fresh (the unit always passes it); anything else
#   is a caller mistake, not a default.
#
# Usage: gate-entry.sh --fresh [--step <comma list of 1-15>]
set -euo pipefail

ONLY_STEP=""
if [[ "${1:-}" == "--fresh" ]]; then
  shift
  if [[ "${1:-}" == "--step" ]]; then
    ONLY_STEP="${2:-}"
    [[ -n "$ONLY_STEP" ]] || { echo "usage: gate-entry.sh --fresh [--step <comma list of 1-15>]" >&2; exit 2; }
    shift 2
  fi
  [[ $# -eq 0 ]] || { echo "usage: gate-entry.sh --fresh [--step <comma list of 1-15>]" >&2; exit 2; }
else
  echo "usage: gate-entry.sh --fresh [--step <comma list of 1-15>]" >&2; exit 2
fi
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
# required names refuse. The handoff names build output dirs, like the
# dev loop's WYRD_OUT/NAK_OUT: the binaries live at $dir/bin.
WYRD_OUT=""; NAK_OUT=""
while IFS= read -r line || [ -n "$line" ]; do
  [ -n "$line" ] || continue
  case "$line" in
    wyrd=*) WYRD_OUT="${line#wyrd=}" ;;
    nak=*) NAK_OUT="${line#nak=}" ;;
  esac
done <"$RUNNERS_ENV"
[ -n "$WYRD_OUT" ] || { echo "gate-entry.sh: runner handoff names no wyrd build" >&2; exit 1; }
[ -n "$NAK_OUT" ] || { echo "gate-entry.sh: runner handoff names no nak build" >&2; exit 1; }
WYRD_BIN="$WYRD_OUT/bin/wyrd"; NAK_BIN="$NAK_OUT/bin/nak"
[ -x "$WYRD_BIN" ] || { echo "gate-entry.sh: wyrd runner is not executable: $WYRD_BIN" >&2; exit 1; }
[ -x "$NAK_BIN" ] || { echo "gate-entry.sh: nak runner is not executable: $NAK_BIN" >&2; exit 1; }

# Run environment, published where the e2e guard parses the token
# and pid (parsed, never sourced) and the legs source the rest. The
# file lives beside the run tree, not in it: the phase-1 guest
# pre-clean empties $RUN, and a file published there would vanish
# mid-suite (as it did). Key parity with the dev loop
# (nix/microvm/run-microvm.sh): the guest legs consume WYRD_BIN,
# RELAY_*, CHECKOUT and E2E_ROOT from this file; a token-and-pid-only
# file starves them under set -u. RELAY_MANAGED stays 0: the relay
# is host-provisioned, never suite-managed.
TOKEN="$(head -c16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
{
  echo "# Generated by tests/gate-entry.sh; sourced by the suite in guests."
  printf 'WYRD_BIN=%q\n' "$WYRD_BIN"
  printf 'RELAY_PORT=%q\n' "$RELAY_PORT"
  printf 'RELAY_HOST=%q\n' "$SUBNET.$RELAY_OCTET"
  printf 'RELAY_MANAGED=%q\n' "0"
  printf 'CHECKOUT=%q\n' "/etc/wyrd-tests"
  printf 'E2E_ROOT=%q\n' "/mnt/wyrd-state/run"
  printf 'WYRD_E2E_TOKEN=%s\n' "$TOKEN"
  printf 'WYRD_E2E_PID=%s\n' "$$"
  printf 'E2E_RUST_LOG=%q\n' "${E2E_RUST_LOG:-}"
  printf 'E2E_INTERVAL=%q\n' "${E2E_INTERVAL:-}"
} > "$STATE_DIR/e2e-env.sh"
chmod 0644 "$STATE_DIR/e2e-env.sh"

# Hold the provision lock for the suite's lifetime (mirrors
# run-microvm.sh): a successful non-blocking acquire by anyone else
# would mean no wrapper is alive. The acquire retries briefly: the
# launcher holds this same lock until the unit starts, so a fast
# unit reaches here while the launcher is still exiting. A lock
# still held past the grace below is genuinely foreign (a second
# run got past pre-flight, or teardown wedged) and still refuses.
exec 9>"$LOCK_FILE"
acquired=""
for _ in $(seq 1 60); do
  if flock -n 9 2>/dev/null; then acquired=1; break; fi
  sleep 1
done
[[ -n "$acquired" ]] || { echo "gate-entry.sh: another run holds $LOCK_FILE" >&2; exit 2; }

export WYRD_E2E_TOKEN="$TOKEN" WYRD_E2E_LOCK="$LOCK_FILE"
export STATE_DIR SSH_KEY WYRD_BIN NAK_BIN PEER_O PEER_N RELAY_URL
# The partition primitive is host-published per task (gate
# suite-wrapper unit env); the entry refuses without it, like the
# legacy host_partition, so fault phases fail at the gate instead
# of stranding taps mid-run.
: "${GATE_PARTITION_BIN:?gate-entry.sh: GATE_PARTITION_BIN is not set (host must publish the tap primitive)}"
export GATE_BATS_LIB GATE_PARTITION_BIN
export E2E_INTERVAL="${E2E_INTERVAL:-}"
# Step selection (`--step 6,10`): phases build on each other, so the
# run set is the prefix closure of the selection — selecting phase N
# runs 1..N, never a lone dependent phase on stale state. Every
# entry must name a real phase, so `--step 99` refuses instead of
# passing zero checks, and the highest entry decides the prefix.
# Grammar shared with the runner and the host orchestrator
# (tests/step-grammar.sh). Microvm steps 1-5 are the shared core
# inside phase 1 (truncated via CORE_ONLY); steps 6-15 are gate
# phases 02-11 (step s lives in file number s-4). The smoke suite
# and the leak-check epilogue always run.
# shellcheck source=tests/step-grammar.sh
source "$ROOT/tests/step-grammar.sh"
MAX_STEP=0
if [[ -n "$ONLY_STEP" ]]; then
  MAX_STEP="$(step_max 1 15 "$ONLY_STEP")" \
    || { echo "gate-entry.sh: --step: '$ONLY_STEP' is not a step (expected a comma list of 1-15)" >&2; exit 1; }
fi
# Shared-core steps selected: the 1..N prefix truncated to the core
# range when the highest selected step falls inside it, as the comma
# list the Lima script already accepts. An absent selection means
# all five.
CORE_MAX=5
if [[ -n "$ONLY_STEP" && "$MAX_STEP" -lt 5 ]]; then CORE_MAX="$MAX_STEP"; fi
CORE_ONLY="1"
for (( _c = 2; _c <= CORE_MAX; _c++ )); do CORE_ONLY="$CORE_ONLY,$_c"; done
unset _c
export CORE_ONLY
# Host $STATE_DIR is shared into every guest at /mnt/wyrd-state (see
# nix/microvm/common.nix), so $STATE_DIR/e2e-env.sh is guest-visible
# at /mnt/wyrd-state/e2e-env.sh. The entry owns that translation: one
# export here covers every phase, which must use $GUEST_ENV, never a
# host path, for the guest-side env file. Beside the run tree, not
# in it: the phase-1 guest pre-clean empties $RUN.
export GUEST_ENV="/mnt/wyrd-state/e2e-env.sh"

# The suite is the prefix closure of the selection: smoke plus phase
# 1 (microvm 1-5, every prefix includes it) plus gate phases 02-11
# for microvm steps 6-15 up to the highest selected step, plus the
# leak-check epilogue (the legacy leak check runs unconditionally,
# and its per-group guards skip unminted credentials on a truncated
# prefix). Absent selection means the whole suite.
SUITE=( "$ROOT/tests/gate-entry-smoke.bats" "$ROOT/tests/gate-phase-01.bats" )
for k in 02 03 04 05 06 07 08 09 10 11; do
  s=$((10#$k + 4))
  if (( MAX_STEP == 0 || s <= MAX_STEP )); then
    SUITE+=( "$ROOT/tests/gate-phase-$k.bats" )
  fi
done
unset k s
SUITE+=( "$ROOT/tests/gate-epilogue.bats" )
exec bats "${SUITE[@]}"
