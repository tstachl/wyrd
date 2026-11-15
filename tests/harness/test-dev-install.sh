#!/usr/bin/env bash
# tests/harness/test-dev-install.sh — pin the one-command developer
# install: the ad-hoc script builds through the wrapper, verifies the
# signature, installs to an Applications dir, and documents the Finder
# boundary. Static assertions only (the real bundle build and the
# install itself are macOS-only manual evidence).
#
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-dev-install.sh`. (Wired into the CI
# shell suites alongside the other stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

INSTALL="$ROOT/packaging/macos/install-dev.sh"
WRAPPER="$ROOT/packaging/macos/build-bundle.sh"

# Code lines only: strip full-line comments before matching, so the
# header prose can never satisfy a pin.
code() { grep -v '^[[:space:]]*#' "$1"; }

# 1. The script exists and is executable.
[[ -x "$INSTALL" ]] \
  || die "install-dev.sh missing or not executable"
pass "install script exists and is executable"

# 2. macOS-only: the .app bundle never ships elsewhere.
code "$INSTALL" | grep -Fq 'uname -s' \
  || die "install: no platform gate"
code "$INSTALL" | grep -Fq 'macOS only' \
  || die "install: no macOS-only refusal"
pass "install script is macOS-only"

# 3. The build delegates to the wrapper: the link-order pin and the
# no-Nix-links guard stay in that one place, never inlined here.
code "$INSTALL" | grep -Fq 'build-bundle.sh' \
  || die "install: build is not delegated to build-bundle.sh"
code "$INSTALL" | grep -Fq 'cargo tauri build' \
  && die "install: inline cargo tauri build duplicates the wrapper"
pass "install builds through the bundle wrapper"

# 4. Fail closed on signature: both the built bundle and the
# installed copy verify before launch is offered.
code "$INSTALL" | grep -Fq 'codesign --verify --deep --strict' \
  || die "install: no codesign verification"
[ "$(code "$INSTALL" | grep -c -F 'codesign --verify --deep --strict')" -ge 2 ] \
  || die "install: built and installed copies are not both verified"
code "$INSTALL" | grep -Fq -- '--deep' \
  || die "install: verify line drifted"
pass "install verifies built and installed copies"

# 5. Installs to an Applications dir: system when writable, user
# fallback, DEST/--dest override. Atomic replace via ditto, no sudo.
code "$INSTALL" | grep -Fq '/Applications/Wyrd.app' \
  || die "install: no /Applications destination"
code "$INSTALL" | grep -Fq 'Applications/Wyrd.app' \
  || die "install: no user Applications fallback"
code "$INSTALL" | grep -Fq -- '--dest' \
  || die "install: no --dest override"
code "$INSTALL" | grep -Fq 'ditto' \
  || die "install: bundle copy is not ditto"
pass "install targets an Applications dir with override"

# 6. Honest Finder boundary in the script output: ad-hoc stays
# inert (no-group-container), team-signed is the separate path.
code "$INSTALL" | grep -Fq 'no-group-container' \
  || die "install: no-group-container boundary missing"
code "$INSTALL" | grep -Fq 'team-signed' \
  || die "install: team-signed path missing"
pass "install states the ad-hoc Finder boundary"

# 7. Prerequisites fail with the fix named: Apple tools, cargo
# (devenv), bun (desktop build), python3 (config version pin).
for marker in 'xcode-select' 'cargo is not on PATH' 'bun is not on PATH' 'python3 is required'; do
  code "$INSTALL" | grep -Fq "$marker" \
    || die "install: prerequisite '$marker' missing"
done
pass "install names its prerequisites with fixes"

# 8. The decision is recorded in hook-adjacent prose, not a bare
# comment: the packaging README names the command, the boundary,
# and the team-signed pointer together.
grep -Fq 'install-dev.sh' "$ROOT/packaging/macos/README.md" \
  || die "README: dev-install command not recorded"
grep -Fq 'no-group-container' "$ROOT/packaging/macos/README.md" \
  || die "README: ad-hoc Finder boundary not recorded"
grep -Fq 'team-signed' "$ROOT/packaging/macos/README.md" \
  || die "README: team-signed pointer not recorded"
pass "README records the command and the Finder boundary"

# 9. The suite itself is wired into the native CI shell job.
grep -Fq 'test-dev-install.sh' "$ROOT/.ngit/nix/workflows/default.nix" \
  || die "CI: dev-install suite not wired into shell.suites"
pass "CI wires the dev-install suite"

echo "dev-install: $PASS checks passed"
