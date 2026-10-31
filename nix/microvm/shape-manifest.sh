#!/usr/bin/env bash
# nix/microvm/shape-manifest.sh — emit the wyrd E2E shape manifest.
#
# The manifest is the handoff to the gate: the exact file list the
# shape pins (harness + suite + evaluation inputs) and the subject
# slot declarations the host policy needs. Generated, never
# hand-maintained: every file set below is extracted from the repo
# the same way tests/harness/test-baked-set.sh extracts it, so the emitter
# cannot silently drop a file the consistency test knows about.
# Output is deterministic (sorted, unique, no timestamps) because
# the approval hashes these bytes.
#
# Usage: bash nix/microvm/shape-manifest.sh
#   (run from anywhere; paths print repo-relative)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# required_from <script>: basenames this script sources or executes.
# Same parser as tests/harness/test-baked-set.sh (kept in sync by
# tests/harness/test-shape-manifest.sh, which fails if they diverge).
required_from() {
  {
    grep -hoE 'source[[:space:]]+[^[:space:];]+' "$1" 2>/dev/null || true
    grep -hoE 'python3[[:space:]]+[^[:space:];]+\.py' "$1" 2>/dev/null || true
    grep -hoE 'bash[[:space:]]+[^[:space:];]+\.sh' "$1" 2>/dev/null || true
  } | sed -E 's#["'"'"']$##; s#^.*[/"'"'"']##' | grep -E '^[A-Za-z0-9_.-]+\.(sh|py)$' || true
}

# Guest-run scripts source each other; the gate entry sources the
# shared grammar. Basenames resolve to their home by existence
# (verified, not assumed): tests/gate for the entry and phases,
# tests/harness for shared helpers, tests/lima for the macOS legs,
# tests/microvm for the guest legs. An unresolvable basename keeps a
# tests/ guess so the test fails loudly on the missing path instead
# of silently dropping it.
locate() {
  for _home in gate harness lima microvm; do
    if [[ -f "$ROOT/tests/$_home/$1" ]]; then
      printf 'tests/%s/%s\n' "$_home" "$1"
      return 0
    fi
  done
  if [[ -f "$ROOT/nix/microvm/$1" ]]; then
    printf 'nix/microvm/%s\n' "$1"
  else
    printf 'tests/%s\n' "$1"
  fi
}
GUEST_SCRIPTS="lima/alpha-lima.sh microvm/alpha-microvm-legs.sh lima/alpha-common.sh"
HOST_SCRIPTS="gate/gate-entry.sh gate/gate-entry-smoke.bats gate/gate-phase-01.bats gate/gate-phase-02.bats gate/gate-phase-03.bats gate/gate-phase-04.bats gate/gate-phase-05.bats gate/gate-phase-06.bats gate/gate-phase-07.bats gate/gate-phase-08.bats gate/gate-phase-09.bats gate/gate-phase-10.bats gate/gate-phase-11.bats gate/gate-epilogue.bats"
HOST_SOURCED=""
FILES=""
for _s in $GUEST_SCRIPTS $HOST_SCRIPTS; do
  for _b in $(required_from "$ROOT/tests/$_s"); do
    FILES="$FILES
$(locate "$_b")"
  done
done
for _s in $HOST_SOURCED; do
  for _b in $(required_from "$ROOT/nix/microvm/$_s"); do
    FILES="$FILES
$(locate "$_b")"
  done
done
unset _s _b
TEST_FILES="$(printf '%s\n' "$FILES" | grep -E '.' | sort -u)"

# Rust workspace inputs: the gate builds packages.wyrd from the staged
# tree (wyrdFor reads ./Cargo.toml at eval time and crane builds the
# workspace from it), so the workspace must be staged. Tracked files
# only, via git ls-files (sorted): target/ and VCS state never enter
# the build, and an untracked file cannot silently slip into the shape.
CARGO="Cargo.toml
Cargo.lock
rust-toolchain.toml
$(git -C "$ROOT" ls-files 'crates')"

# Baked set: the repo files common.nix bakes into /etc/wyrd-tests,
# read from the .source targets so the manifest follows the layout
# instead of guessing it (the basename rule the consistency tests use
# keeps matching: they suffix-match against these paths).
BAKED="$(grep -hoE '\.source = \.\./\.\./tests/[^; ]+' "$ROOT/nix/microvm/common.nix" \
  | sed -E 's#.*\.\./\.\./##' | sort -u)"

# Harness files: evaluated or executed by the host directly (literal
# list, not extracted — the test asserts every entry exists).
# tests/lima/ files are the host-side wrapper the guest suite requires
# (alpha-lima.sh runs wrapped); step-5 review N1. The gate closure
# (entry, bats phases, helpers) is the host-executed suite: listed
# literally because the extractor only follows source|bash|python3
# targets and would miss the entry points themselves.
HARNESS="flake.nix
flake.lock
tests/lima/provision.sh
tests/lima/run-alpha.sh
tests/lima/wyrd-alpha.yaml
nix/microvm/empty-src
nix/microvm/common.nix
nix/microvm/image.nix
nix/microvm/image-manifest.schema.json
nix/microvm/validate-manifest.py
nix/microvm/peer-o.nix
nix/microvm/peer-n.nix
nix/microvm/relay.nix
tests/harness/e2e-guard.sh
tests/harness/e2e-helpers.bash
tests/harness/step-grammar.sh
tests/gate/gate-entry.sh
tests/gate/gate-entry-smoke.bats
tests/gate/gate-phase-01.bats
tests/gate/gate-phase-02.bats
tests/gate/gate-phase-03.bats
tests/gate/gate-phase-04.bats
tests/gate/gate-phase-05.bats
tests/gate/gate-phase-06.bats
tests/gate/gate-phase-07.bats
tests/gate/gate-phase-08.bats
tests/gate/gate-phase-09.bats
tests/gate/gate-phase-10.bats
tests/gate/gate-phase-11.bats
tests/gate/gate-epilogue.bats"

{
  echo "# wyrd E2E shape manifest — generated by nix/microvm/shape-manifest.sh, do not hand-edit."
  echo "# regenerate: bash nix/microvm/shape-manifest.sh (output is deterministic: sorted, unique, no timestamps)"
  echo "[shape-files]"
  printf '%s\n%s\n%s\n%s\n' "$TEST_FILES" "$BAKED" "$HARNESS" "$CARGO" | grep -E '.' | sort -u
  echo "[subject-slots]"
  # The slot text is a claim the host enforces: keep it in sync
  # with the gate provisioner's subject ingestion (the
  # --override-input wyrdSrc + IFD-denied + offline invocation) and
  # the wyrdSrcFor routing in flake.nix. A flag removed there must
  # be removed here (N9).
  echo "wyrdSrc: source-tree input (path:, narHash-pinned via --override-input); built by base-owned wyrdFor under pure eval + IFD-denied + offline; the subject tree's own flake/lockfile/modules are never evaluated and no PR Nix is evaluated anywhere"
}
