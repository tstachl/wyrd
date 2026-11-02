#!/usr/bin/env bash
# Hermetic pipeline test for shape-manifest.sh: feed a fixture listing
# (including glob-hostile and dotted names) through
# SHAPE_MANIFEST_CARGO and require every line back verbatim. No git,
# no network, no guest.
#
# Proves the emitter pipeline (printf/grep/sort) preserves entries
# byte-for-byte, including names that break naive glob handling. The
# committed-tree sourcing itself is pinned textually in
# test-shape-manifest.sh; the production path (no seam) is exercised
# by that suite.
#
# Run from the checkout: bash tests/harness/test-shape-manifest-pipeline.sh
# Exits nonzero on any failure.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "ok: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1"; }

# Fixture: ordinary entries plus glob-hostile and dotted names.
FIXTURE="Cargo.toml
Cargo.lock
crates/br[ack]et/lib.rs
crates/star*/build.rs
crates/sp ace/mod.rs
crates/dotted.v2/lib.rs"

MANIFEST="$(SHAPE_MANIFEST_CARGO="$FIXTURE" bash "$ROOT/nix/microvm/shape-manifest.sh")"
[ -n "$MANIFEST" ] || { bad "emitter produced no output for the fixture"; }

UNPINNED=""
while IFS= read -r _f; do
  [ -n "$_f" ] || continue
  printf '%s\n' "$MANIFEST" | grep -qxF "$_f" || UNPINNED="$UNPINNED $_f"
done <<< "$FIXTURE"
if [ -z "$UNPINNED" ]; then
  ok "fixture entries round-trip verbatim"
else
  bad "emitter dropped fixture entries:$UNPINNED"
fi
unset _f

# Determinism on the pinned input.
MANIFEST2="$(SHAPE_MANIFEST_CARGO="$FIXTURE" bash "$ROOT/nix/microvm/shape-manifest.sh")"
if [ "$MANIFEST" == "$MANIFEST2" ]; then
  ok "deterministic on pinned input"
else
  bad "emitter output differs between runs on identical input"
fi

# The seam must bypass git entirely: with a failing git first on
# PATH, the pinned run still succeeds (and the strict-mode probe in
# test-shape-manifest.sh covers the unpinned path).
FAILBIN="$(mktemp -d "${TMPDIR:-/tmp}/shape-seam-test.XXXXXXXX")"
[ -d "$FAILBIN" ] || { bad "scratch dir setup failed"; }
trap 'rm -rf "$FAILBIN"' EXIT
printf '#!%s\nexit 1\n' "$(command -v sh)" >"$FAILBIN/git"
chmod +x "$FAILBIN/git"
if PATH="$FAILBIN:$PATH" SHAPE_MANIFEST_CARGO="$FIXTURE" bash "$ROOT/nix/microvm/shape-manifest.sh" >/dev/null 2>&1; then
  ok "seam bypasses git"
else
  bad "emitter called git despite the seam"
fi
rm -rf "$FAILBIN"
trap - EXIT

echo "---"
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
