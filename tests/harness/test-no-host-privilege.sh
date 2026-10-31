#!/usr/bin/env bash
# tests/harness/test-no-host-privilege.sh — enumeration test for the P0
# zero-host-privilege migration (complete: the legacy root-unit path
# is deleted, so this pins the end state, not the migration).
#
# No repository code executed on Odin with host privilege may name a
# privileged program: the repository contributes orchestration (data
# and parameters), the host contributes the privilege through
# capabilities (GATE_GUEST_BIN, GATE_PARTITION_BIN). Pure bash, no host, no nix: run it
# anywhere with `bash tests/harness/test-no-host-privilege.sh`. (Wired into
# the CI shell-tests job alongside the other stubs.)
#
# Scope: the host-executed task closure — the files the gate task
# runs on the host as the build user (tests/gate/gate-entry.sh plus the
# smoke suite and bats phases it execs). Guest-executed legs
# (alpha-microvm-legs.sh and what they source) run as the guest user
# inside the VMs; guest-side sudo is guest privilege, not host
# privilege, and stays.
#
# Discipline: the bans below match comments too, so a comment that
# needs one of these words must be reworded, not excepted. Guest
# payloads are the only exception: a line routing through on_o/on_n
# (or guest_exec, the gate-owned agent transport both route through)
# executes inside the guest over the agent, where sudo/mount/chown are
# guest privilege.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

CLOSURE=(
  "$ROOT/tests/harness/e2e-guard.sh"
  "$ROOT/tests/harness/e2e-helpers.bash"
  "$ROOT/tests/harness/step-grammar.sh"
  "$ROOT/tests/gate/gate-entry.sh"
  "$ROOT/tests/gate/gate-entry-smoke.bats"
  "$ROOT/tests/gate/gate-phase-01.bats"
  "$ROOT/tests/gate/gate-phase-02.bats"
  "$ROOT/tests/gate/gate-phase-03.bats"
  "$ROOT/tests/gate/gate-phase-04.bats"
  "$ROOT/tests/gate/gate-phase-05.bats"
  "$ROOT/tests/gate/gate-phase-06.bats"
  "$ROOT/tests/gate/gate-phase-07.bats"
  "$ROOT/tests/gate/gate-phase-08.bats"
  "$ROOT/tests/gate/gate-phase-09.bats"
  "$ROOT/tests/gate/gate-phase-10.bats"
  "$ROOT/tests/gate/gate-phase-11.bats"
  "$ROOT/tests/gate/gate-epilogue.bats"
)
for f in "${CLOSURE[@]}"; do
  [[ -f "$f" ]] || die "task closure file missing: $f"
done
pass "task closure files exist"

# --- the closure sources only the closure ---
while IFS= read -r line; do
  case "$line" in
    *step-grammar.sh*|*e2e-guard.sh*|*e2e-helpers.bash*) ;;
    *) die "task closure sources outside itself: $line" ;;
  esac
done < <(grep -h -E '^[[:space:]]*(source|\.) ' "${CLOSURE[@]}" || true)
pass "task closure sources only itself"

# --- no privileged program names in the closure ---
# Words banned everywhere except guest payloads (on_o/on_n and
# guest_exec lines — both route through the gate-owned agent
# transport, so the named program runs in the guest, never on the
# host); phrases banned everywhere (they have no guest use).
WORDS="setpriv|chown|mount|umount|sudo|qemu|virtiofsd|pgrep|pkill|ssh-keygen"
PHRASES="ip link|ip tuntap|ip addr|nix build|nix-store"
for f in "${CLOSURE[@]}"; do
  # `GUEST_BIN" exec` is the entry's direct transport invocation (same
  # gate-owned binary the helpers wrap); anything else naming the bin
  # (exports, assignments) carries no privileged token.
  hits="$(grep -n -w -E "$WORDS" "$f" | grep -v 'on_o\|on_n\|guest_exec\|GUEST_BIN" exec' || true)"
  [[ -z "$hits" ]] || die "privileged token in ${f#$ROOT/} (guest payloads excepted):\n$hits"
  phits="$(grep -n -E "$PHRASES" "$f" || true)"
  [[ -z "$phits" ]] || die "privileged operation in ${f#$ROOT/}:\n$phits"
done
pass "task closure names no privileged program"

# --- kill -0 (liveness) is the only kill ---
# `kill -0` proves a leg is alive without signalling it; any other
# kill form is host process management and belongs below the
# task-runner boundary.
for f in "${CLOSURE[@]}"; do
  khits="$(grep -n -w 'kill' "$f" | grep -v 'kill -0' || true)"
  [[ -z "$khits" ]] || die "non-liveness kill in ${f#$ROOT/}:\n$khits"
done
pass "task closure kills nothing (kill -0 liveness only)"

# --- the capability surface is defined, not bypassed ---
# The agent transport and the partition primitive live behind
# GATE_GUEST_BIN / GATE_PARTITION_BIN, published by the gate entry
# and consumed through e2e-helpers.bash (on_o/on_n, host_partition,
# wait_guest_file) and the gate-owned bats helpers the phases load.
grep -q 'GATE_GUEST_BIN' "$ROOT/tests/harness/e2e-helpers.bash" \
  || die "guest transport capability (GATE_GUEST_BIN) missing from the shims"
grep -q 'GATE_GUEST_BIN' "$ROOT/tests/gate/gate-entry.sh" \
  || die "guest transport capability (GATE_GUEST_BIN) not published by the entry"
grep -q 'GATE_PARTITION_BIN' "$ROOT/tests/harness/e2e-helpers.bash" \
  || die "partition capability (GATE_PARTITION_BIN) missing from the shims"
pass "capability surface (guest transport, partition) is defined"

echo "no-host-privilege: $PASS checks passed"
