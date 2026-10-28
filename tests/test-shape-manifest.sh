#!/usr/bin/env bash
# tests/test-shape-manifest.sh — the shape manifest handoff stays
# complete and deterministic: every file the consistency checks know
# about appears in nix/microvm/shape-manifest.sh output, every listed
# path exists, and two runs are byte-identical (the approval hashes
# these bytes). Pure bash + grep, no guest, no nix.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

MANIFEST="$(bash "$ROOT/nix/microvm/shape-manifest.sh")"
[[ -n "$MANIFEST" ]] || die "manifest emitter produced no output"

# Determinism: run twice, byte-identical.
MANIFEST2="$(bash "$ROOT/nix/microvm/shape-manifest.sh")"
[[ "$MANIFEST" == "$MANIFEST2" ]] || die "manifest output is not deterministic"
pass "manifest output is deterministic"

# Every listed shape file exists.
MISSING=""
while IFS= read -r line; do
  [[ "$line" == shape-files:* || "$line" == "["* || "$line" == \#* || -z "$line" ]] && continue
  # shellcheck disable=SC2076
  [[ "$line" =~ ^[A-Za-z0-9_./-]+\.(nix|nix\.lock|sh|bash|bats|py|lock|toml)$ ]] || continue
  [[ -f "$ROOT/$line" ]] || MISSING="$MISSING $line"
done <<< "$(printf '%s\n' "$MANIFEST" | sed -n '/^\[shape-files\]/,/^\[subject-slots\]/p')"
[[ -z "$MISSING" ]] || die "manifest lists missing files:$MISSING"
pass "every listed shape file exists"

# Coverage: every guest-required and baked basename (the sets
# tests/test-baked-set.sh enforces) appears in the manifest.
required_from() {
  {
    grep -hoE 'source[[:space:]]+[^[:space:];]+' "$1" 2>/dev/null || true
    grep -hoE 'python3[[:space:]]+[^[:space:];]+\.py' "$1" 2>/dev/null || true
  } | sed -E 's#["'"'"']$##; s#^.*[/"'"'"']##' | grep -E '^[A-Za-z0-9_.-]+\.(sh|py)$' || true
}
REQUIRED=""
for _s in alpha-lima.sh alpha-microvm-legs.sh alpha-common.sh; do
  REQUIRED="$REQUIRED
$(required_from "$SCRIPT_DIR/$_s")"
done
unset _s
BAKED="$(grep -hoE '"wyrd-tests/[^"]+"' "$ROOT/nix/microvm/common.nix" \
  | sed -E 's#"wyrd-tests/##; s#"$##' | sort -u)"
UNCOVERED=""
for _f in $(printf '%s\n%s\n' "$REQUIRED" "$BAKED" | grep -E '.' | sort -u); do
  printf '%s\n' "$MANIFEST" | grep -qE "(^|/)${_f}\$" || UNCOVERED="$UNCOVERED $_f"
done
[[ -z "$UNCOVERED" ]] || die "manifest omits required files:$UNCOVERED"
pass "manifest covers every required and baked file"

# Rust workspace inputs: the gate builds packages.wyrd from the staged
# tree, so every tracked Cargo input must be pinned. Inverse check:
# list the tracked workspace files from git and require each in the
# manifest (a build input the manifest drops fails the build at eval).
UNPINNED=""
while IFS= read -r _f; do
  [ -n "$_f" ] || continue
  printf '%s\n' "$MANIFEST" | grep -qxF "$_f" || UNPINNED="$UNPINNED $_f"
done < <(git -C "$ROOT" ls-files Cargo.toml Cargo.lock rust-toolchain.toml 'crates')
[[ -z "$UNPINNED" ]] || die "manifest omits Cargo inputs:$UNPINNED"
pass "manifest pins every tracked Cargo input"

# Subject slot declared.
printf '%s\n' "$MANIFEST" | grep -q '^\[subject-slots\]$' || die "manifest lacks a [subject-slots] section"
printf '%s\n' "$MANIFEST" | grep -q '^wyrdSrc:' || die "manifest lacks the wyrdSrc slot declaration"
pass "subject slot declared"

echo "shape-manifest: $PASS checks passed"
