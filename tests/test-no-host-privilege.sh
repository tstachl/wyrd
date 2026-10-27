#!/usr/bin/env bash
# tests/test-no-host-privilege.sh — enumeration test for the P0
# zero-host-privilege migration.
#
# No repository code executed on Odin with host privilege may name a
# privileged program: the repository contributes orchestration (data
# and parameters), the host contributes the privilege through
# capabilities (GATE_AS_GUEST_BIN, GATE_PARTITION_BIN,
# GATE_HAND_TO_GUEST_BIN). Pure bash, no host, no nix: run it
# anywhere with `bash tests/test-no-host-privilege.sh`. (Wired into
# the CI shell-tests job alongside the other stubs.)
#
# Scope: the host-executed task closure — the files the gate task
# runs on the host (tests/alpha-microvm.sh plus the two modules it
# sources, and the gate-path entry: tests/gate-entry.sh plus the
# smoke suite it execs, which run as the build user on Odin). Guest-executed legs (alpha-microvm-legs.sh and what they
# source) run as the guest user inside the VMs; guest-side sudo is
# guest privilege, not host privilege, and stays. Dev-loop-only
# files (nix/microvm/run-microvm.sh, provision-host.sh,
# build-account.sh, dev-*.sh) keep their privilege openly: they run
# as root on the developer's own machine (its own trust domain) and
# are never executed on Odin via the task path.
#
# Discipline (same precedent as tests/test-provision-host.sh): the
# bans below match comments too, so a comment that needs one of
# these words must be reworded, not excepted. Guest payloads are the
# only exception: a line routing through on_o/on_n executes inside
# the guest over ssh, where sudo/mount/chown are guest privilege.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

CLOSURE=(
  "$ROOT/tests/alpha-microvm.sh"
  "$ROOT/tests/e2e-guard.sh"
  "$ROOT/tests/e2e-helpers.bash"
  "$ROOT/tests/step-grammar.sh"
  "$ROOT/tests/gate-entry.sh"
  "$ROOT/tests/gate-entry-smoke.bats"
  "$ROOT/tests/gate-phase-01.bats"
  "$ROOT/tests/gate-phase-02.bats"
  "$ROOT/tests/gate-phase-03.bats"
  "$ROOT/tests/gate-phase-04.bats"
  "$ROOT/tests/gate-phase-05.bats"
  "$ROOT/tests/gate-phase-06.bats"
  "$ROOT/tests/gate-phase-07.bats"
  "$ROOT/tests/gate-phase-08.bats"
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
# Words banned everywhere except guest payloads (on_o/on_n lines);
# phrases banned everywhere (they have no guest use).
WORDS="setpriv|chown|mount|umount|sudo|qemu|virtiofsd|pgrep|pkill|ssh-keygen"
PHRASES="ip link|ip tuntap|ip addr|nix build|nix-store"
for f in "${CLOSURE[@]}"; do
  hits="$(grep -n -w -E "$WORDS" "$f" | grep -v 'on_o\|on_n' || true)"
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
grep -q 'GATE_AS_GUEST_BIN' "$ROOT/tests/alpha-microvm.sh" \
  || die "as_guest capability (GATE_AS_GUEST_BIN) missing from the suite"
grep -q 'GATE_PARTITION_BIN' "$ROOT/tests/alpha-microvm.sh" \
  || die "partition capability (GATE_PARTITION_BIN) missing from the suite"
grep -q 'GATE_HAND_TO_GUEST_BIN' "$ROOT/tests/alpha-microvm.sh" \
  || die "handover capability (GATE_HAND_TO_GUEST_BIN) missing from the suite"
pass "capability surface (as-guest, partition, handover) is defined"

# --- the dev loop provides every capability the suite requires ---
for cap in GATE_AS_GUEST_BIN GATE_PARTITION_BIN GATE_HAND_TO_GUEST_BIN; do
  grep -q "\"$cap=" "$ROOT/nix/microvm/run-microvm.sh" \
    || die "dev loop does not provide $cap (suite would refuse)"
done
pass "dev loop provides every suite capability"

echo "no-host-privilege: $PASS checks passed"
