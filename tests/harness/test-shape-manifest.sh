#!/usr/bin/env bash
# tests/harness/test-shape-manifest.sh — the shape manifest handoff stays
# complete and deterministic: every file the consistency checks know
# about appears in nix/microvm/shape-manifest.sh output, every listed
# path exists, and two runs are byte-identical (the approval hashes
# these bytes). Pure bash + grep, no guest, no nix.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

# The Cargo listing comes from the committed tree, not the mutable
# index: `git ls-files` reads the index, which the provider rewrites
# while materializing the checkout, and consecutive reads have
# disagreed in CI. Tree objects are immutable, so the oracle below
# cannot flap. The emitter runs the production path (no seam), so
# this compares two independent derivations of the same committed
# set: a mismatch is a real defect, failed fast with no retry.
EXPECTED="$(git -C "$ROOT" ls-tree -r HEAD --name-only -- Cargo.toml Cargo.lock rust-toolchain.toml crates)" || die "git ls-tree failed"
[[ -n "$EXPECTED" ]] || die "git ls-tree returned empty"
printf '%s\n' "$EXPECTED" | grep -qxF Cargo.toml || die "git ls-tree capture is missing Cargo.toml itself"
# The emitter's default sourcing must stay the committed tree: this
# textual pin fails the suite if anyone narrows or reverts it to a
# live-index read, which would reopen the flake this file exists to
# prevent. (It asserts the command, not its output: the output is
# covered by the inverse check below and the hermetic pipeline test.)
# shellcheck disable=SC2016
# (pattern is intentionally literal)
grep -qxF '  CARGO="$(git -C "$ROOT" ls-tree -r HEAD --name-only -- Cargo.toml Cargo.lock rust-toolchain.toml crates)"' "$ROOT/nix/microvm/shape-manifest.sh" || die "emitter sourcing is not the committed tree"
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
# tests/harness/test-baked-set.sh enforces) appears in the manifest.
required_from() {
  {
    grep -hoE 'source[[:space:]]+[^[:space:];]+' "$1" 2>/dev/null || true
    grep -hoE 'python3[[:space:]]+[^[:space:];]+\.py' "$1" 2>/dev/null || true
  } | sed -E 's#["'"'"']$##; s#^.*[/"'"'"']##' | grep -E '^[A-Za-z0-9_.-]+\.(sh|py)$' || true
}
REQUIRED=""
for _s in ../lima/alpha-lima.sh ../microvm/alpha-microvm-legs.sh ../lima/alpha-common.sh; do
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
# tree, so every tracked Cargo input must be pinned. Inverse check
# against the committed tree captured above: a build input the manifest
# drops fails the build at eval. Both sides derive from immutable tree
# objects (the emitter's default read, this check's oracle read), so a
# mismatch is a real defect and fails fast with no retry.
UNPINNED=""
while IFS= read -r _f; do
  [ -n "$_f" ] || continue
  printf '%s\n' "$MANIFEST" | grep -qxF "$_f" || UNPINNED="$UNPINNED $_f"
done <<< "$EXPECTED"
[[ -z "$UNPINNED" ]] || {
  echo "diagnostics for the Cargo-inputs mismatch (manifest omits committed inputs):" >&2
  echo "  HEAD: $(git -C "$ROOT" log --oneline -1 2>&1 || true)" >&2
  echo "  git: $(git --version 2>&1 || true)" >&2
  echo "  oracle listing count: $(printf '%s\n' "$EXPECTED" | grep -c . || true)" >&2
  for _f in $UNPINNED; do
    # shellcheck disable=SC2086
    echo "  recheck ${_f}: tree has it: [$(git -C "$ROOT" ls-tree -r HEAD --name-only -- "$_f" 2>&1 || true)]; on disk: $([[ -f "$ROOT/$_f" ]] && echo yes || echo no)" >&2
  done
  unset _f
  die "manifest omits Cargo inputs:$UNPINNED"
}
pass "manifest pins every tracked Cargo input"

# Subject slot declared.
printf '%s\n' "$MANIFEST" | grep -q '^\[subject-slots\]$' || die "manifest lacks a [subject-slots] section"
printf '%s\n' "$MANIFEST" | grep -q '^wyrdSrc:' || die "manifest lacks the wyrdSrc slot declaration"
pass "subject slot declared"

# Strict mode, pinned literally and live: a nixpkgs-fmt-style
# reformat once rewrote `set -euo pipefail` into `set - euo pipefail`
# (positional parameters, strict mode silently off). The literal pin
# catches the rewrite; the live probe proves an unguarded input
# failure (here: git) fails the emitter instead of emitting a
# manifest with silently empty inputs.
grep -q '^set -euo pipefail$' "$ROOT/nix/microvm/shape-manifest.sh" \
  || die "emitter does not run under strict mode (set -euo pipefail)"
pass "emitter runs under strict mode"
FAILBIN="$(mktemp -d)"
trap 'rm -rf "$FAILBIN"' EXIT
printf '#!%s\nexit 1\n' "$(command -v sh)" >"$FAILBIN/git"
chmod +x "$FAILBIN/git"
if PATH="$FAILBIN:$PATH" bash "$ROOT/nix/microvm/shape-manifest.sh" >/dev/null 2>&1; then
  die "emitter succeeded with a failing git (strict mode is off)"
fi
rm -rf "$FAILBIN"
trap - EXIT
pass "emitter fails when an input command fails"

echo "shape-manifest: $PASS checks passed"
