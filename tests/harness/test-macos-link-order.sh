#!/usr/bin/env bash
# tests/harness/test-macos-link-order.sh — pin the macOS SDK
# link-order fix: the team-signed bundle must link no /nix/store
# path (hardened runtime rejects the foreign-team Nix libiconv at
# launch), so bundle builds go through the wrapper that pins the SDK
# /usr/lib first and guards the result.
#
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-macos-link-order.sh`. (Wired into the CI
# shell-tests job alongside the other stubs; the real bundle build
# and otool proof stay macOS-only manual evidence.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

WRAPPER="$ROOT/packaging/macos/build-bundle.sh"

# 1. The wrapper exists and is executable.
[[ -x "$WRAPPER" ]] \
  || die "build-bundle.sh missing or not executable"
pass "wrapper exists and is executable"

# 2. Darwin-only pin: SDK discovery through the system xcrun (never
# a bare PATH lookup the Nix shadow can hijack), the link-arg that
# makes the SDK libiconv.tbd win, guarded by the tbd's presence.
grep -Fq '/usr/bin/xcrun' "$WRAPPER" \
  || die "wrapper: SDK discovery bypasses the /usr/bin xcrun"
grep -Fq 'link-arg=-L' "$WRAPPER" \
  || die "wrapper: no link-arg SDK /usr/lib pin"
grep -Fq 'libiconv.tbd' "$WRAPPER" \
  || die "wrapper: pin is not tied to the SDK libiconv.tbd"
pass "wrapper pins the SDK /usr/lib link-arg on Darwin"

# 3. The pin appends to (never replaces) caller RUSTFLAGS and only
# on Darwin; elsewhere the build forwards untouched.
grep -Fq 'RUSTFLAGS="${RUSTFLAGS:-}' "$WRAPPER" \
  || die "wrapper: RUSTFLAGS is replaced instead of appended"
grep -Fq 'uname -s' "$WRAPPER" \
  || die "wrapper: no Darwin gate for the pin"
pass "wrapper appends RUSTFLAGS on Darwin, forwards elsewhere"

# 4. The wrapper builds through the Tauri hook path from the desktop
# crate (the hook paths assume that CWD) and defaults to the
# runbook shape `--bundles app`.
grep -Fq 'cargo tauri build' "$WRAPPER" \
  || die "wrapper: no cargo tauri build invocation"
grep -Fq 'crates/wyrd-desktop' "$WRAPPER" \
  || die "wrapper: build does not run from crates/wyrd-desktop"
grep -Fq -- '--bundles app' "$WRAPPER" \
  || die "wrapper: default is not --bundles app"
pass "wrapper builds through cargo tauri from the desktop crate"

# 5. No-Nix-links guard: otool over the bundle binary (plus the
# standalone binary when present) refuses any /nix/store linkage.
grep -Fq 'otool -L' "$WRAPPER" \
  || die "wrapper: no otool linkage guard"
grep -Fq '/nix/store' "$WRAPPER" \
  || die "wrapper: guard names no /nix/store refusal"
pass "wrapper guards the built binary against /nix/store links"

# 6. The decision is recorded where the macOS packaging lives.
grep -Fq 'build-bundle.sh' "$ROOT/packaging/macos/README.md" \
  || die "README: wrapper not recorded"
grep -Fq '/nix/store' "$ROOT/packaging/macos/README.md" \
  || die "README: no-Nix-links rationale not recorded"
pass "README records the wrapper and the guard"

# 7. The release runbook builds through the wrapper (the step that
# names the Tauri hook), keeping one bundle entry point.
grep -Fq 'build-bundle.sh' "$ROOT/.ngit/release.yaml" \
  || die "runbook: bundle step does not go through the wrapper"
pass "runbook builds through the wrapper"

echo "macos-link-order: $PASS checks passed"
