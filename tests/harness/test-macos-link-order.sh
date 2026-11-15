#!/usr/bin/env bash
# tests/harness/test-macos-link-order.sh — pin the macOS SDK
# link-order fix: the team-signed bundle must link no /nix/store
# path (hardened runtime rejects the foreign-team Nix libiconv at
# launch), so bundle builds go through the wrapper that pins the SDK
# /usr/lib first and guards the result.
#
# Static pins match code lines only (comments stripped), with an
# encoded negative control proving the stripping; the guard itself
# is exercised behaviorally with a stub otool plus fixture binaries
# covering the standalone, bundle-glob, appex, custom-dir, and empty
# legs. Pure bash, no host, no nix: run it anywhere with
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

# 5. The wrapper delegates the guard to the check script with a
# release dir that follows CARGO_TARGET_DIR instead of inlining
# its own linkage walk.
code "$WRAPPER" | grep -Fq 'check-no-nix-links.sh' \
  || die "wrapper: guard is not delegated to check-no-nix-links.sh"
code "$WRAPPER" | grep -Fq 'CARGO_TARGET_DIR' \
  || die "wrapper: release dir does not follow CARGO_TARGET_DIR"
pass "wrapper delegates the guard with a relocatable release dir"

# 6. The check script resolves the shipped binaries without a
# hardcoded bundle binary name (the app's Contents/MacOS glob),
# covers the standalone binary and the embedded appex binary,
# honors a CARGO_TARGET_DIR relocation plus an explicit release
# dir, captures each otool run once, and fails closed when it
# inspects nothing.
code "$CHECK" | grep -Fq 'Contents/MacOS' \
  || die "check: no bundle MacOS glob"
code "$CHECK" | grep -Fq 'WyrdFileProvider.appex' \
  || die "check: appex binary not covered"
code "$CHECK" | grep -Fq 'CARGO_TARGET_DIR' \
  || die "check: no target-dir relocation"
code "$CHECK" | grep -Fq "case \"\$LINKED\" in" \
  || die "check: linkage is not classified from a single capture"
code "$CHECK" | grep -Fq 'inspected nothing' \
  || die "check: empty inspection is not a failure"
pass "check resolves binaries, covers the appex, fails closed"

# 7. Negative control: the comment-stripping is load-bearing, not
# decorative. A wrapper copy whose `--bundles app` default survives
# only as a comment (the header prose mentions it) must fail the
# corresponding pin — proving a prose mention can never satisfy it.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
sed '/^[[:space:]]*set -- --bundles app$/d' "$WRAPPER" > "$TMP/wrapper-comment-only.sh"
code "$TMP/wrapper-comment-only.sh" | grep -Fq -- '--bundles app' \
  && die "negative control: comment-only mention satisfied the pin"
pass "comment-only mentions cannot satisfy pins"

# 8. Behavioral: the guard passes a clean tree, fails Nix-linked
# binaries on every leg, honors a relocated release dir, and fails
# an empty tree — through a stub otool, so this runs on Linux CI
# too. The stub answers per path: any binary with `bad` in its
# name reports a Nix link, the rest follow the modal fixture.
mkdir -p "$TMP/bin"
cat > "$TMP/bin/otool" <<'EOF'
#!/bin/sh
case "$2" in
*bad*) cat "$OTOOL_NIX_OUT" ;;
*) cat "$OTOOL_MODE_OUT" ;;
esac
EOF
chmod +x "$TMP/bin/otool"
export PATH="$TMP/bin:$PATH"
export OTOOL_NIX_OUT="$TMP/nix.out" OTOOL_MODE_OUT="$TMP/mode.out"
nix_out() { printf '\t/nix/store/abc-libiconv/lib/libiconv.2.dylib (compatibility version 7.0.0)\n'; }
clean_out() { printf '\t/usr/lib/libiconv.2.dylib (compatibility version 7.0.0)\n'; }
nix_out > "$OTOOL_NIX_OUT"

fixture_root() { rm -rf "$TMP/root"; mkdir -p "$TMP/root/target/release"; }
clean_out > "$OTOOL_MODE_OUT"

fixture_root
touch "$TMP/root/target/release/wyrd-desktop"
"$CHECK" "$TMP/root" >/dev/null 2>&1 \
  || die "check: clean tree refused"
pass "guard passes a clean tree"

clean_out > "$OTOOL_MODE_OUT"
fixture_root
touch "$TMP/root/target/release/wyrd-desktop"
OTOOL_MODE_OUT="$OTOOL_NIX_OUT" "$CHECK" "$TMP/root" >/dev/null 2>&1 \
  && die "check: Nix-linked standalone passed"
pass "guard fails a Nix-linked standalone binary"

fixture_root
"$CHECK" "$TMP/root" >/dev/null 2>&1 \
  && die "check: empty tree passed without inspecting anything"
pass "guard fails closed on an empty tree"

# The bundle leg is name-independent: a binary carrying the
# documented-but-wrong `Wyrd` name is still inspected — clean
# passes, Nix-linked fails.
fixture_root
mkdir -p "$TMP/root/target/release/bundle/macos/Wyrd.app/Contents/MacOS"
touch "$TMP/root/target/release/bundle/macos/Wyrd.app/Contents/MacOS/Wyrd"
clean_out > "$OTOOL_MODE_OUT"
"$CHECK" "$TMP/root" >/dev/null 2>&1 \
  || die "check: clean bundle binary under another name refused"
pass "guard inspects the bundle binary under any name"

fixture_root
mkdir -p "$TMP/root/target/release/bundle/macos/Wyrd.app/Contents/MacOS"
touch "$TMP/root/target/release/bundle/macos/Wyrd.app/Contents/MacOS/Wyrd-bad"
clean_out > "$OTOOL_MODE_OUT"
"$CHECK" "$TMP/root" >/dev/null 2>&1 \
  && die "check: Nix-linked bundle binary passed"
pass "guard fails a Nix-linked bundle binary"

# The appex leg: with only the extension binary present, clean
# passes (proving it counts as inspected — without appex coverage
# the guard would fail closed) and Nix-linked fails.
fixture_root
APPEX_MACOS="$TMP/root/target/release/bundle/macos/Wyrd.app/Contents/PlugIns/WyrdFileProvider.appex/Contents/MacOS"
mkdir -p "$APPEX_MACOS"
touch "$APPEX_MACOS/WyrdFileProvider"
clean_out > "$OTOOL_MODE_OUT"
"$CHECK" "$TMP/root" >/dev/null 2>&1 \
  || die "check: clean appex binary refused"
pass "guard inspects the appex binary"
OTOOL_MODE_OUT="$OTOOL_NIX_OUT" "$CHECK" "$TMP/root" >/dev/null 2>&1 \
  && die "check: Nix-linked appex binary passed"
pass "guard fails a Nix-linked appex binary"

# A relocated release dir (CARGO_TARGET_DIR or explicit arg) is
# honored instead of tripping the fail-closed guard.
fixture_root
mkdir -p "$TMP/custom/release"
touch "$TMP/custom/release/wyrd-desktop"
clean_out > "$OTOOL_MODE_OUT"
"$CHECK" "$TMP/root" "$TMP/custom/release" >/dev/null 2>&1 \
  || die "check: explicit release dir refused"
CARGO_TARGET_DIR="$TMP/custom" "$CHECK" "$TMP/root" >/dev/null 2>&1 \
  || die "check: CARGO_TARGET_DIR relocation refused"
pass "guard honors a relocated release dir"

echo "macos-link-order: $PASS checks passed"
