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

# The Cargo listing is captured twice up front and must agree before
# anything is compared: the emitter and the check below both consume
# this one agreed listing through SHAPE_MANIFEST_CARGO (a test seam in
# nix/microvm/shape-manifest.sh; production callers get the live git
# read). Comparing two live reads flakes in CI, where the read has
# dropped between one and five entries while an identical re-read
# lists all 443 — worker-side read degradation, never reproduced
# locally. A listing that never agrees across 3 rounds is worker
# flakiness, failed loudly with the git version.
PREV=""
AGREED=""
ATTEMPT=0
while [ "$ATTEMPT" -lt 3 ]; do
  ATTEMPT=$((ATTEMPT + 1))
  LISTING="$(git -C "$ROOT" ls-files Cargo.toml Cargo.lock rust-toolchain.toml 'crates')" || die "git ls-files failed (agreement attempt $ATTEMPT/3)"
  [[ -n "$LISTING" ]] || die "git ls-files returned empty (agreement attempt $ATTEMPT/3)"
  printf '%s\n' "$LISTING" | grep -qxF Cargo.toml || die "git ls-files capture is missing Cargo.toml itself (agreement attempt $ATTEMPT/3)"
  if [[ -n "$PREV" ]]; then
    if [[ "$LISTING" == "$PREV" ]]; then
      AGREED="$LISTING"
      break
    fi
    if [ "$ATTEMPT" -lt 3 ]; then
      echo "  workspace listing moved between reads (attempt $ATTEMPT/3); re-capturing after a 5s settle delay..." >&2
      sleep 5
    fi
  fi
  PREV="$LISTING"
done
[[ -n "$AGREED" ]] || die "workspace listing never agreed across 3 attempts (git $(git --version 2>&1 || true))"
EXPECTED="$AGREED"
unset PREV AGREED LISTING ATTEMPT
MANIFEST="$(SHAPE_MANIFEST_CARGO="$EXPECTED" bash "$ROOT/nix/microvm/shape-manifest.sh")"
[[ -n "$MANIFEST" ]] || die "manifest emitter produced no output"

# Determinism: run twice, byte-identical (same agreed input, so this
# exercises only the emitter pipeline, never git).
MANIFEST2="$(SHAPE_MANIFEST_CARGO="$EXPECTED" bash "$ROOT/nix/microvm/shape-manifest.sh")"
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
# against the agreed listing captured above: a build input the manifest
# drops fails the build at eval. The emitter ran on this exact listing
# through SHAPE_MANIFEST_CARGO, so a mismatch here is a real emitter
# defect and fails fast with no retry.
UNPINNED=""
while IFS= read -r _f; do
  [ -n "$_f" ] || continue
  printf '%s\n' "$MANIFEST" | grep -qxF "$_f" || UNPINNED="$UNPINNED $_f"
done <<< "$EXPECTED"
[[ -z "$UNPINNED" ]] || {
  echo "diagnostics for the Cargo-inputs mismatch (emitter dropped pinned inputs):" >&2
  echo "  HEAD: $(git -C "$ROOT" log --oneline -1 2>&1 || true)" >&2
  echo "  git: $(git --version 2>&1 || true)" >&2
  echo "  status: $(git -C "$ROOT" status --porcelain 2>&1 | head -20 || true)" >&2
  echo "  agreed listing count: $(printf '%s\n' "$EXPECTED" | grep -c . || true)" >&2
  for _f in $UNPINNED; do
    # shellcheck disable=SC2086
    echo "  recheck ${_f}: still listed: [$(git -C "$ROOT" ls-files -- "$_f" 2>&1 || true)]; on disk: $([[ -f "$ROOT/$_f" ]] && echo yes || echo no)" >&2
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
