#!/usr/bin/env bash
# tests/harness/test-nix-harness.sh — pin that CI actually executes the
# evaluation-only harness checks. Pure bash + grep: no builds, no nix,
# no guests. Run from the checkout: bash tests/harness/test-nix-harness.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKFLOW="$ROOT/.ngit/nix/workflows/default.nix"
MODULE="$SCRIPT_DIR/test-wyrd-module.sh"
MANIFEST="$SCRIPT_DIR/test-image-manifest.sh"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

# The shell suites job must supply nix in its hermetic runtimeInputs.
# Without it both nix-evaluating harness files silently skipped.
BLOCK="$(sed -n '/shell\.suites = pkgs\.writeShellApplication {/,/^    \];/p' "$WORKFLOW")"
[ -n "$BLOCK" ] || die "shell.suites runtimeInputs block not found"
printf '%s\n' "$BLOCK" | grep >/dev/null '^      nix$' \
  || die "shell suites do not provide nix (evaluation checks would skip)"
pass "shell suites provide nix"

# Missing nix must fail closed unless the skip is explicitly requested.
grep >/dev/null 'WYRD_SKIP_NIX_EVAL' "$MODULE" \
  || die "module harness has no explicit skip opt-out"
if grep -E '^command -v nix .*exit 0' "$MODULE" >/dev/null; then
  die "module harness still skips silently when nix is absent"
fi
if ! grep >/dev/null 'FAIL: nix is required' "$MODULE"; then
  die "module harness does not fail when nix is absent"
fi
pass "module harness fails closed without an explicit opt-out"

# Both nix-evaluating harness files keep their checks.
grep >/dev/null 'nix eval --json' "$MODULE" || die "module harness lost its nix eval checks"
grep >/dev/null 'nix eval --json' "$MANIFEST" || die "image-manifest lost its nix eval cross-check"
pass "evaluation checks remain in both harness files"

# The manifest harness must fail closed too, not silently omit its
# cross-checks on a runner without nix/jq.
grep >/dev/null 'WYRD_SKIP_NIX_EVAL' "$MANIFEST" \
  || die "manifest harness has no explicit skip opt-out"
if grep >/dev/null 'SKIP: nix or jq unavailable' "$MANIFEST"; then
  die "manifest harness still skips silently when nix is absent"
fi
if ! grep >/dev/null 'FAIL: nix and jq are required' "$MANIFEST"; then
  die "manifest harness does not fail when nix is absent"
fi
pass "manifest harness fails closed without an explicit opt-out"

# nix_eval must keep stderr off the JSON it hands to python3. A cold
# guest emits warnings that corrupt 2>&1-merged stdout.
if grep >/dev/null 'nix eval --json "$@" 2>&1' "$MODULE"; then
  die "module harness merges nix stderr into its JSON stdout"
fi
grep >/dev/null '2>"$err_file"' "$MODULE" \
  || die "module harness does not separate nix stderr from JSON"
pass "module harness keeps nix stderr off the JSON stream"

# The current CLI argv is pinned, not the old vault spelling.
grep >/dev/null "' owner /var/lib/wyrd/demo '" "$MODULE" \
  || die "module harness still pins the retired vault argv"
pass "systemd argv assertion uses the owner subcommand"

# This pin must itself be wired into the CI shell suites.
grep >/dev/null 'test-nix-harness.sh' "$WORKFLOW" \
  || die "nix harness pin is not wired into shell suites"
pass "nix harness pin is wired into shell suites"

echo "nix-harness: $PASS checks passed"
