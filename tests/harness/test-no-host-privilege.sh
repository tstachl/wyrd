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
# Elevation (sudo) is checked separately and narrowly: the unit
# holds no privilege itself and reaches the two host-published
# capabilities (GATE_GUEST_BIN, GATE_PARTITION_BIN) only through a
# passwordless host sudo grant (see README.md: the grant names exactly
# those binaries, and every call uses -n and fails fast without it).
# The exemption below names only the wrapper resolution itself
# (sudo_bin, wrappers/bin/sudo), the exact elevated call sites
# (through entry_sudo_bin/e2e_sudo_bin), and guest payloads
# (on_o/on_n run inside the guest, where sudo is guest privilege).
# Anything else naming sudo — including a line that merely mentions
# a capability beside a host sudo — refuses.
WORDS="setpriv|chown|mount|umount|qemu|virtiofsd|pgrep|pkill|ssh-keygen"
PHRASES="ip link|ip tuntap|ip addr|nix build|nix-store"
# Production elevation-scan shapes, defined once and shared by the
# scans and their self-test below, so a regex typo cannot drift one
# without failing the other.
SUDO_SCAN_V='on_o\|on_n\|guest_exec\|sudo_bin\|wrappers/bin/sudo'
INVOC_PAT='[$]\((entry|e2e)_sudo_bin\)"'
NONFLAG_OK='sudo_bin)" -n'
UNQ_PAT='[$]\((entry|e2e)_sudo_bin\)([^"]|$)|=\s*"?\$\((entry|e2e)_sudo_bin\)'
# cap_ok <line>: the elevation names its capability.
cap_ok() {
  case "$1" in
    *GATE_GUEST_BIN*|*GATE_PARTITION_BIN*|*GUEST_BIN*) return 0 ;;
    *) return 1 ;;
  esac
}
for f in "${CLOSURE[@]}"; do
  # `GUEST_BIN" exec` is the entry's direct transport invocation (same
  # gate-owned binary the helpers wrap); anything else naming the bin
  # (exports, assignments) carries no privileged token.
  hits="$(grep -n -w -E "$WORDS" "$f" | grep -v 'on_o\|on_n\|guest_exec\|GUEST_BIN" exec\|GUEST_BIN" push' || true)"
  [[ -z "$hits" ]] || die "privileged token in ${f#$ROOT/} (guest payloads excepted):\n$hits"
  phits="$(grep -n -E "$PHRASES" "$f" || true)"
  [[ -z "$phits" ]] || die "privileged operation in ${f#$ROOT/}:\n$phits"
  shits="$(grep -n -w 'sudo' "$f" | grep -v "$SUDO_SCAN_V" || true)"
  [[ -z "$shits" ]] || die "host elevation outside the capabilities in ${f#$ROOT/}:\n$shits"
done
pass "task closure names no privileged program"
# Elevated call sites must name their capability: every line that
# elevates through the helpers must also name GATE_GUEST_BIN,
# GATE_PARTITION_BIN, or the entry GUEST_BIN alias, so a sudo aimed
# anywhere else cannot hide behind the exemption above.
while IFS= read -r _line; do
  cap_ok "$_line" || die "elevation without a capability target: $_line"
done < <(grep -n -E "$INVOC_PAT" "${CLOSURE[@]}" || true)
unset _line
pass "every elevation names its capability"
# Every helper-mediated elevation must fail fast (-n on each call):
# without it a missing grant would prompt instead of refusing, and
# the suite would hang on the host instead of failing at the gate.
nonflag="$(grep -n -E "$INVOC_PAT" "${CLOSURE[@]}" | grep -v -F "$NONFLAG_OK" || true)"
[[ -z "$nonflag" ]] || die "elevation without -n on the helper itself (would prompt on a missing grant):\n$nonflag"
pass "every elevation fails fast"
# Quoted invocations ("$(..._sudo_bin)") are the only shape the scans
# cover; an unquoted or indirected one would escape, so refuse it.
unquoted="$(grep -n -E "$UNQ_PAT" "${CLOSURE[@]}" || true)"
[[ -z "$unquoted" ]] || die "unquoted or indirected helper use escapes the scans:\n$unquoted"
pass "every helper invocation is quoted"

# --- the scans are live, not vacuous ---
# A regex typo that matches nothing would pass every check above
# while guarding zero lines (exactly how the `\\?` bug survived one
# round). The probes below run the production variables and function,
# never copies: known-bad shapes must flag, known-good shapes must
# stay clean, and the tree must hold live invocations to examine.
[[ "$(grep -h -E "$INVOC_PAT" "${CLOSURE[@]}" | wc -l)" -ge 5 ]] \
  || die "elevation scans are vacuous (no invocations in the closure)"
printf '%s\n' 'sudo rm -rf / # GUEST_BIN' | grep -w 'sudo' | grep -v "$SUDO_SCAN_V" \
  | grep -q . || die "sudo scan misses hostile sudo with a capability mention"
printf '%s\n' '  if [ -x /run/wrappers/bin/sudo ]; then printf x; fi # sudo_bin fallback' \
  | grep -w 'sudo' | grep -v "$SUDO_SCAN_V" | grep -q . \
  && die "sudo scan flags the wrapper resolution"
printf '%s\n' '  run on_o "sudo mkfs.ext4" # guest payload' \
  | grep -w 'sudo' | grep -v "$SUDO_SCAN_V" | grep -q . \
  && die "sudo scan flags an in-guest payload"
cap_ok 'timeout 5 "$(e2e_sudo_bin)" -n /bin/true' \
  && die "capability check passes an elevation without a target"
cap_ok 'timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec x' \
  || die "capability check flags a legitimate elevated call"
printf '%s\n' 'timeout 5 "$(e2e_sudo_bin)" "$GATE_GUEST_BIN" exec tail -n 3' \
  | grep -E "$INVOC_PAT" | grep -v -F "$NONFLAG_OK" | grep -q . \
  || die "fail-fast scan misses a payload-masked missing -n"
printf '%s\n' 'x=$(e2e_sudo_bin)' \
  | grep -E "$UNQ_PAT" | grep -q . \
  || die "quote scan misses a bare end-of-line assignment"
printf '%s\n' 'bin="$(e2e_sudo_bin)"' \
  | grep -E "$UNQ_PAT" | grep -q . \
  || die "quote scan misses a quoted assignment (the \\? regression)"
printf '%s\n' 'timeout 120 "$(entry_sudo_bin)" -n "$GUEST_BIN" exec x' \
  | grep -E "$UNQ_PAT" | grep -q . \
  && die "quote scan flags a legitimate quoted call"
pass "the scans flag hostile shapes and spare legitimate ones"

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
