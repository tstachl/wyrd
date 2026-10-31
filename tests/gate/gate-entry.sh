#!/usr/bin/env bash
# tests/gate/gate-entry.sh — gate-path wrapper for the wyrd e2e suite.
#
# The entry point the gate provisioned unit runs: as the build user,
# on the traversable build copy, with the GATE_* capability env.
# Root provisioning (bridge, builds, import, boot, agent wait)
# belongs to the host; the host already provisioned, built, booted,
# and waited, so this wrapper only assembles the suite environment,
# mints the run token, holds the provision lock, prepares the guests,
# and execs bats on the suite files.
#
# Contract (pinned by tests/harness/test-gate-entry.sh):
# - Reads, never derives: provision datum files (state-dir, lock-file,
#   relay-role, relay-port) and the validated image manifests at
#   $PROVISION_DIR/manifests/<build>.json (root-written,
#   build-user-readable). The guest IPs come from the manifests'
#   role/ip fields — never from derived subnets or tap octets.
#   A missing or malformed input refuses here, before any suite
#   phase runs.
# - Datum schema: relay-port is 1-65535; relay-role names a manifest
#   role present in the manifests dir. Shape violations refuse like
#   missing files do.
# - The suite reaches guests only through GATE_GUEST_BIN (mandatory,
#   unset refuses): guest_exec/push/pull from the gate-owned bats
#   helpers. There is no ssh and no shared filesystem; the Incus
#   agent carries control and files.
# - WYRD_BIN/NAK_BIN are the stable in-image paths: the images carry
#   both packages, so no runner handoff resolution exists.
# - The published run env file carries the keys the guest legs
#   consume (WYRD_BIN, NAK_BIN, RELAY_*, peer IPs, CHECKOUT, E2E_ROOT
#   plus token, owner pid and debug passthroughs). It is pushed into
#   each peer guest; the legs address it as GUEST_ENV.
# - Holds the provision lock for the suite's lifetime, so the e2e
#   guard's lock check and a later teardown agree on who owns the
#   run. The initial acquire retries briefly: the launcher releases
#   its own hold only as the unit starts, so a fast unit can reach
#   this acquire while the launcher is still exiting.
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
command -v jq >/dev/null || { echo "gate-entry.sh: jq is not on PATH" >&2; exit 1; }
command -v timeout >/dev/null || { echo "gate-entry.sh: timeout is not on PATH" >&2; exit 1; }

PROVISION_DIR="${GATE_TASK_PROVISION_DIR:?gate-entry.sh: GATE_TASK_PROVISION_DIR is not set}"
BATS_LIB="${GATE_BATS_LIB:?gate-entry.sh: GATE_BATS_LIB is not set}"
[ -d "$BATS_LIB" ] || { echo "gate-entry.sh: bats helpers are missing at $BATS_LIB" >&2; exit 1; }
GUEST_BIN="${GATE_GUEST_BIN:?gate-entry.sh: GATE_GUEST_BIN is not set (host must publish the guest transport)}"
[ -x "$GUEST_BIN" ] || { echo "gate-entry.sh: guest transport is not executable: $GUEST_BIN" >&2; exit 1; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

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
RELAY_ROLE="$(policy relay-role)" || exit 1
RELAY_PORT="$(policy relay-port)" || exit 1
# Shape, not just presence: a malformed datum would otherwise build
# bad endpoints silently. The host validates these too when it
# publishes them; this is the entry's fail-fast refusal.
[[ "$RELAY_PORT" =~ ^[0-9]{1,5}$ ]] && [ "$RELAY_PORT" -ge 1 ] && [ "$RELAY_PORT" -le 65535 ] \
  || { echo "gate-entry.sh: relay port out of bounds: $RELAY_PORT" >&2; exit 1; }
[[ "$RELAY_ROLE" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]] \
  || { echo "gate-entry.sh: relay role out of bounds: $RELAY_ROLE" >&2; exit 1; }
RUN="$STATE_DIR/run"
[ -d "$RUN" ] || { echo "gate-entry.sh: run tree is missing at $RUN (host provisions it)" >&2; exit 1; }

# Guest IPs from the validated image manifests the host published
# (one <build>.json per image build, role-addressed inside). The
# entry names no build and derives no subnet: the manifests are the
# single declaration of addressing.
MANIFESTS_DIR="$PROVISION_DIR/manifests"
[ -d "$MANIFESTS_DIR" ] || { echo "gate-entry.sh: provision manifests are missing at $MANIFESTS_DIR" >&2; exit 1; }
O_IP=""; N_IP=""; RELAY_IP=""
for m in "$MANIFESTS_DIR"/*.json; do
  [ -f "$m" ] || { echo "gate-entry.sh: no image manifests in $MANIFESTS_DIR" >&2; exit 1; }
  role="$(jq -r '.role // empty' "$m" 2>/dev/null || true)"
  ip="$(jq -r '.ip // empty' "$m" 2>/dev/null || true)"
  [[ "$role" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]] || { echo "gate-entry.sh: manifest role out of bounds in $m" >&2; exit 1; }
  [[ "$ip" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}/24$ ]] || { echo "gate-entry.sh: manifest ip out of bounds in $m: $ip" >&2; exit 1; }
  ip="${ip%/*}"
  _o1="${ip%%.*}"; _rest="${ip#*.}"; _o2="${_rest%%.*}"; _rest="${_rest#*.}"; _o3="${_rest%%.*}"; _o4="${ip##*.}"
  for _o in "$_o1" "$_o2" "$_o3" "$_o4"; do
    [ "$_o" -le 255 ] || { echo "gate-entry.sh: manifest ip octet out of bounds in $m: $ip" >&2; exit 1; }
  done
  unset _o1 _o2 _o3 _o4 _o _rest
  case "$role" in
    peer-o) O_IP="$ip" ;;
    peer-n) N_IP="$ip" ;;
  esac
  if [ "$role" = "$RELAY_ROLE" ]; then RELAY_IP="$ip"; fi
done
[ -n "$O_IP" ] || { echo "gate-entry.sh: manifests name no peer-o role" >&2; exit 1; }
[ -n "$N_IP" ] || { echo "gate-entry.sh: manifests name no peer-n role" >&2; exit 1; }
[ -n "$RELAY_IP" ] || { echo "gate-entry.sh: manifests name no $RELAY_ROLE role" >&2; exit 1; }
RELAY_URL="ws://$RELAY_IP:$RELAY_PORT"

# In-image binaries: the images carry the wyrd subject and nak, so
# these stable NixOS paths resolve identically on every guest with
# no handoff. The smoke suite asserts executability in-guest before
# anything else runs.
WYRD_BIN="/run/current-system/sw/bin/wyrd"
NAK_BIN="/run/current-system/sw/bin/nak"

# Run environment, published where the e2e guard parses the token
# and pid (parsed, never sourced) and pushed into each peer guest,
# which the legs address as GUEST_ENV. The guest legs consume
# WYRD_BIN, NAK_BIN, RELAY_*, peer IPs, CHECKOUT and E2E_ROOT from
# this file; a token-and-pid-only file starves them under set -u.
# RELAY_MANAGED stays 0: the relay is host-provisioned, never
# suite-managed.
E2E_ROOT="/var/lib/wyrd-e2e"
TOKEN="$(head -c16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
{
  echo "# Generated by tests/gate/gate-entry.sh; pushed into each peer guest."
  printf 'WYRD_BIN=%q\n' "$WYRD_BIN"
  printf 'NAK_BIN=%q\n' "$NAK_BIN"
  printf 'RELAY_PORT=%q\n' "$RELAY_PORT"
  printf 'RELAY_HOST=%q\n' "$RELAY_IP"
  printf 'RELAY_MANAGED=%q\n' "0"
  printf 'CHECKOUT=%q\n' "/etc/wyrd-tests"
  printf 'E2E_ROOT=%q\n' "$E2E_ROOT"
  printf 'WYRD_PEER_O_IP=%q\n' "$O_IP"
  printf 'WYRD_PEER_N_IP=%q\n' "$N_IP"
  printf 'RENDEZVOUS_PORT=%q\n' "18770"
  printf 'WYRD_E2E_TOKEN=%s\n' "$TOKEN"
  printf 'WYRD_E2E_PID=%s\n' "$$"
  printf 'E2E_RUST_LOG=%q\n' "${E2E_RUST_LOG:-}"
  printf 'E2E_INTERVAL=%q\n' "${E2E_INTERVAL:-}"
} > "$RUN/e2e-env.sh"
chmod 0644 "$RUN/e2e-env.sh"

# Guest preparation: fresh state trees on both peers, owned by the
# suite user the legs run as, then the env file where the legs
# address it. The full layout (drives, creds, mounts, logs) is
# created here because only phase 1 wipes and recreates it, and
# phase 1 runs on peer-o alone — a peer-n leg must never hit a
# missing LOGDIR. The agent is the only transport; a guest that never
# answers fails the run here, before the suite.
for peer in peer-o peer-n; do
  timeout 120 "$GUEST_BIN" exec "$peer" -- mkdir -p "$E2E_ROOT" \
    || { echo "gate-entry.sh: guest $peer is unreachable (agent exec failed)" >&2; exit 1; }
  timeout 120 "$GUEST_BIN" exec "$peer" -- rm -rf "$E2E_ROOT" \
    || { echo "gate-entry.sh: guest $peer state reset failed" >&2; exit 1; }
  timeout 120 "$GUEST_BIN" exec "$peer" -- mkdir -p "$E2E_ROOT/drives" "$E2E_ROOT/creds" "$E2E_ROOT/mnt" "$E2E_ROOT/logs" \
    || { echo "gate-entry.sh: guest $peer state reset failed" >&2; exit 1; }
  timeout 120 "$GUEST_BIN" exec "$peer" -- chown -R e2e:users "$E2E_ROOT" \
    || { echo "gate-entry.sh: guest $peer state handover failed" >&2; exit 1; }
  timeout 120 "$GUEST_BIN" push "$peer" "$RUN/e2e-env.sh" "$E2E_ROOT/e2e-env.sh" \
    || { echo "gate-entry.sh: env push to $peer failed" >&2; exit 1; }
done

# Hold the provision lock for the suite's lifetime: a successful
# non-blocking acquire by anyone else would mean no wrapper is
# alive. The acquire retries briefly: the
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
export STATE_DIR WYRD_BIN NAK_BIN RELAY_URL
# The partition primitive is host-published per task (gate
# suite-wrapper unit env); the entry refuses without it, like the
# guest transport, so fault phases fail at the gate instead
# of stranding instances mid-run.
: "${GATE_PARTITION_BIN:?gate-entry.sh: GATE_PARTITION_BIN is not set (host must publish the partition primitive)}"
export GATE_BATS_LIB GATE_PARTITION_BIN GATE_GUEST_BIN
export E2E_INTERVAL="${E2E_INTERVAL:-}"
# Step selection (`--step 6,10`): phases build on each other, so the
# run set is the prefix closure of the selection — selecting phase N
# runs 1..N, never a lone dependent phase on stale state. Every
# entry must name a real phase, so `--step 99` refuses instead of
# passing zero checks, and the highest entry decides the prefix.
# Grammar shared with the runner and the host orchestrator
# (tests/harness/step-grammar.sh). Microvm steps 1-5 are the shared core
# inside phase 1 (truncated via CORE_ONLY); steps 6-15 are gate
# phases 02-11 (step s lives in file number s-4). The smoke suite
# and the leak-check epilogue always run.
# shellcheck source=tests/harness/step-grammar.sh
source "$ROOT/tests/harness/step-grammar.sh"
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
# The env file lives at the same guest path on both peers; the entry
# owns that translation: one export here covers every phase, which
# must use $GUEST_ENV, never a host path, for the guest-side env file.
export GUEST_ENV="$E2E_ROOT/e2e-env.sh"
export GUEST_RUN="$E2E_ROOT"
export GUEST_TESTS="/etc/wyrd-tests"

# The suite is the prefix closure of the selection: smoke plus phase
# 1 (microvm 1-5, every prefix includes it) plus gate phases 02-11
# for microvm steps 6-15 up to the highest selected step, plus the
# leak-check epilogue (the legacy leak check runs unconditionally,
# and its per-group guards skip unminted credentials on a truncated
# prefix). Absent selection means the whole suite.
SUITE=( "$ROOT/tests/gate/gate-entry-smoke.bats" "$ROOT/tests/gate/gate-phase-01.bats" )
for k in 02 03 04 05 06 07 08 09 10 11; do
  s=$((10#$k + 4))
  if (( MAX_STEP == 0 || s <= MAX_STEP )); then
    SUITE+=( "$ROOT/tests/gate/gate-phase-$k.bats" )
  fi
done
unset k s
SUITE+=( "$ROOT/tests/gate/gate-epilogue.bats" )
exec bats "${SUITE[@]}"
