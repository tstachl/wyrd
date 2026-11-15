#!/usr/bin/env bash
# tests/harness/test-macos-link-order.sh — pin the macOS SDK
# link-order fix: the team-signed bundle must link no /nix/store
# path (hardened runtime rejects the foreign-team Nix libiconv at
# launch), so bundle builds go through the wrapper that pins the SDK
# /usr/lib first and guards the result.
#
# Static pins match code lines only (comments stripped), so a prose
# mention can never satisfy them; the guard itself is exercised
# behaviorally with a stub otool plus fixture binaries. Pure bash, no
# host, no nix: run it anywhere with
# `bash tests/harness/test-macos-link-order.sh`. (Wired into the CI
# shell suites alongside the other stubs; the real bundle build
# and otool proof stay macOS-only manual evidence.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

WRAPPER="$ROOT/packaging/macos/build-bundle.sh"
CHECK="$ROOT/packaging/macos/check-no-nix-links.sh"

# Code lines only: strip full-line comments before matching, so the
# header prose can never satisfy a pin. (Inline trailing comments
# stay; the pinned strings are executable code, not commentary.)
code() { grep -v '^[[:space:]]*#' "$1"; }

# 1. Both scripts exist and are executable.
[[ -x "$WRAPPER" ]] \
  || die "build-bundle.sh missing or not executable"
[[ -x "$CHECK" ]] \
  || die "check-no-nix-links.sh missing or not executable"
pass "wrapper and guard exist and are executable"

# 2. Darwin-only pin: SDK discovery through the system xcrun (never
# a bare PATH lookup the Nix shadow can hijack), the link-arg that
# makes the SDK libiconv.tbd win, guarded by the tbd's presence.
code "$WRAPPER" | grep -Fq '/usr/bin/xcrun' \
  || die "wrapper: SDK discovery bypasses the /usr/bin xcrun"
code "$WRAPPER" | grep -Fq 'link-arg=-L' \
  || die "wrapper: no link-arg SDK /usr/lib pin"
code "$WRAPPER" | grep -Fq 'libiconv.tbd' \
  || die "wrapper: pin is not tied to the SDK libiconv.tbd"
pass "wrapper pins the SDK /usr/lib link-arg on Darwin"

# 3. The pin appends to (never replaces) caller RUSTFLAGS, refuses a
# whitespace-bearing SDK path RUSTFLAGS cannot carry, and gates on
# Darwin only; elsewhere the build forwards untouched.
code "$WRAPPER" | grep -Fq 'RUSTFLAGS="${RUSTFLAGS:-}' \
  || die "wrapper: RUSTFLAGS is replaced instead of appended"
code "$WRAPPER" | grep -Fq '[[:space:]]' \
  || die "wrapper: whitespace SDK path is not refused"
code "$WRAPPER" | grep -Fq 'uname -s' \
  || die "wrapper: no Darwin gate for the pin"
pass "wrapper appends RUSTFLAGS, refuses whitespace, gates on Darwin"

# 4. The wrapper builds through the Tauri hook path from the desktop
# crate (the hook paths assume that CWD) and defaults to the
# runbook shape `--bundles app`.
code "$WRAPPER" | grep -Fq 'cargo tauri build' \
  || die "wrapper: no cargo tauri build invocation"
code "$WRAPPER" | grep -Fq 'crates/wyrd-desktop' \
  || die "wrapper: build does not run from crates/wyrd-desktop"
code "$WRAPPER" | grep -Fq -- '--bundles app' \
  || die "wrapper: default is not --bundles app"
pass "wrapper builds through cargo tauri from the desktop crate"

# 5. The wrapper delegates the guard to the check script instead of
# inlining its own linkage walk.
code "$WRAPPER" | grep -Fq 'check-no-nix-links.sh' \
  || die "wrapper: guard is not delegated to check-no-nix-links.sh"
pass "wrapper delegates the guard to the check script"

# 6. The check script resolves the shipped binaries without a
# hardcoded binary name (the app's Contents/MacOS glob), covers the
# standalone binary and the embedded appex binary, captures each
# otool run once, and fails closed when it inspects nothing.
code "$CHECK" | grep -Fq 'Contents/MacOS' \
  || die "check: no bundle MacOS glob"
code "$CHECK" | grep -Fq 'WyrdFileProvider.appex' \
  || die "check: appex binary not covered"
code "$CHECK" | grep -Fq "case \"\$LINKED\" in" \
  || die "check: linkage is not classified from a single capture"
code "$CHECK" | grep -Fq 'inspected nothing' \
  || die "check: empty inspection is not a failure"
pass "check resolves binaries, covers the appex, fails closed"

# 7. Behavioral: the guard passes a clean tree, fails a Nix-linked
# binary, and fails an empty tree — through a stub otool, so this
# runs on Linux CI too.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin"
cat > "$TMP/bin/otool" <<'EOF'
#!/bin/sh
cat "$OTOOL_FIXTURE_OUT"
EOF
chmod +x "$TMP/bin/otool"
export PATH="$TMP/bin:$PATH"

fixture_root() { rm -rf "$TMP/root"; mkdir -p "$TMP/root/target/release"; }
clean_out() { printf '%s:\n\t/usr/lib/libiconv.2.dylib (compatibility version 7.0.0)\n' "$1" > "$TMP/out"; }
nix_out() { printf '%s:\n\t/nix/store/abc-libiconv/lib/libiconv.2.dylib (compatibility version 7.0.0)\n' "$1" > "$TMP/out"; }
export OTOOL_FIXTURE_OUT="$TMP/out"

fixture_root
touch "$TMP/root/target/release/wyrd-desktop"
clean_out "$TMP/root/target/release/wyrd-desktop"
"$CHECK" "$TMP/root" >/dev/null 2>&1 \
  || die "check: clean tree refused"
pass "guard passes a clean tree"

fixture_root
touch "$TMP/root/target/release/wyrd-desktop"
nix_out "$TMP/root/target/release/wyrd-desktop"
"$CHECK" "$TMP/root" >/dev/null 2>&1 \
  && die "check: Nix-linked binary passed"
pass "guard fails a Nix-linked binary"

fixture_root
"$CHECK" "$TMP/root" >/dev/null 2>&1 \
  && die "check: empty tree passed without inspecting anything"
pass "guard fails closed on an empty tree"

echo "macos-link-order: $PASS checks passed"
