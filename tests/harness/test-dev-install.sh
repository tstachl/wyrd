#!/usr/bin/env bash
# tests/harness/test-dev-install.sh — pin the one-command developer
# install: the ad-hoc script builds through the wrapper, stages the
# co-installed CLI the desktop resolves first, verifies the
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
STAGE_CLI="$ROOT/packaging/macos/stage-cli.sh"
CONF="$ROOT/crates/wyrd-desktop/tauri.conf.json"
PROVISION="$ROOT/crates/wyrd-desktop/src/provision.rs"

# Code lines only, captured once: strip full-line comments before
# matching, so the header prose can never satisfy a pin, and match
# with a here-string (a `code | grep -q` pipeline under pipefail
# would report 141 once grep exits early and the script grows past
# the pipe buffer).
INSTALL_CODE="$(grep -v '^[[:space:]]*#' "$INSTALL")"
STAGE_CODE="$(grep -v '^[[:space:]]*#' "$STAGE_CLI")"
CONF_CODE="$(cat "$CONF")"
PROVISION_CODE="$(cat "$PROVISION")"
match() { grep -F -q -- "$1" <<<"$2"; }

# 1. The scripts exist and are executable.
[[ -x "$INSTALL" ]] \
  || die "install-dev.sh missing or not executable"
[[ -x "$STAGE_CLI" ]] \
  || die "stage-cli.sh missing or not executable"
pass "install and stage scripts exist and are executable"

# 2. macOS-only: the .app bundle never ships elsewhere.
match 'uname -s' "$INSTALL_CODE" \
  || die "install: no platform gate"
match 'macOS only' "$INSTALL_CODE" \
  || die "install: no macOS-only refusal"
pass "install script is macOS-only"

# 3. The build delegates to the wrapper: the link-order pin and the
# no-Nix-links guard stay in that one place, never inlined here. The
# hook stages the appex plus the CLI before the outer sign.
match 'build-bundle.sh' "$INSTALL_CODE" \
  || die "install: build is not delegated to build-bundle.sh"
match 'cargo tauri build' "$INSTALL_CODE" \
  && die "install: inline cargo tauri build duplicates the wrapper"
match 'stage-cli.sh' "$CONF_CODE" \
  || die "config: CLI stage is not in beforeBundleCommand"
match 'Resources/wyrd' "$CONF_CODE" \
  || die "config: bundled CLI mapping missing"
match '--bundles app' "$INSTALL_CODE" \
  || die "install: app-only default missing"
pass "install builds through the wrapper with the CLI staged"

# 3b. The wrapper reseals the outer app shallowly after the build:
# the bundler's linker-only seal leaves the staged files
# unverified, and only a shallow re-sign (never `--deep`, which
# would re-sign the appex without its entitlements) makes the
# installed copy verify deep-strict.
WRAPPER_CODE="$(grep -v '^[[:space:]]*#' "$ROOT/packaging/macos/build-bundle.sh")"
match 'codesign --force --sign' "$WRAPPER_CODE" \
  || die "wrapper: no post-bundle reseal"
match 'bundle/macos/Wyrd.app' "$WRAPPER_CODE" \
  || die "wrapper: reseal names no bundle path"
pass "wrapper reseals the outer app shallowly"

# 4. The desktop resolves the bundled CLI first: the fixed pair the
# acceptance case (create a drive, show it Available) depends on.
match 'bundled_cli_path' "$PROVISION_CODE" \
  || die "provision: bundled CLI resolution missing"
match 'Resources' "$PROVISION_CODE" \
  || die "provision: Resources lookup missing"
match 'Contents/Resources/wyrd' "$INSTALL_CODE" \
  || die "install: bundled CLI presence is not verified"
pass "bundled CLI is staged, mapped, and resolved first"

# 5. Fail closed on signature: the built bundle verifies before the
# install touches the destination, and the staged copy verifies
# before it replaces the old app.
match 'codesign --verify --deep --strict' "$INSTALL_CODE" \
  || die "install: no codesign verification"
[ "$(grep -c -F 'codesign --verify --deep --strict' <<<"$INSTALL_CODE")" -ge 3 ] \
  || die "install: built, staged, and installed copies are not all verified"
pass "install verifies built, staged, and installed copies"

# 6. Installs to an Applications dir: system when writable, the
# user's Applications fallback as its own pattern, DEST/--dest
# override, no sudo. Replace rides a staging name: the old app is
# removed only after the new copy verifies.
match '/Applications/Wyrd.app' "$INSTALL_CODE" \
  || die "install: no /Applications destination"
match '"$HOME/Applications/Wyrd.app"' "$INSTALL_CODE" \
  || die "install: no user Applications fallback"
match '--dest' "$INSTALL_CODE" \
  || die "install: no --dest override"
match 'ditto' "$INSTALL_CODE" \
  || die "install: bundle copy is not ditto"
match '.new' "$INSTALL_CODE" \
  || die "install: no staging-name replace"
pass "install targets an Applications dir with staged replace"

# 6b. The release-dir rule mirrors the wrapper (CARGO_TARGET_DIR,
# triple from --target/CARGO_BUILD_TARGET); the two copies are
# pinned together so the installer inspects what the guard guarded.
for marker in 'CARGO_TARGET_DIR' 'CARGO_BUILD_TARGET' '--target=*)'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: release-dir rule '$marker' missing"
  match "$marker" "$WRAPPER_CODE" \
    || die "wrapper: release-dir rule '$marker' missing"
done
pass "installer mirrors the wrapper release-dir rule"

# 7. Honest Finder boundary in the script output: ad-hoc stays
# inert (no-group-container), team-signed is the separate path.
match 'no-group-container' "$INSTALL_CODE" \
  || die "install: no-group-container boundary missing"
match 'team-signed' "$INSTALL_CODE" \
  || die "install: team-signed path missing"
pass "install states the ad-hoc Finder boundary"

# 8. Prerequisites fail with the fix named: Apple tools, cargo
# (devenv), bun (desktop build), python3 (config version pin).
for marker in 'xcode-select' 'cargo is not on PATH' 'bun is not on PATH' 'python3 is required'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: prerequisite '$marker' missing"
done
pass "install names its prerequisites with fixes"

# 9. The stage script builds the release CLI with the SDK pin,
# signs it with the paired identity, and refuses cross-target
# embeds — mirroring the appex stage, never the outer bundle. The
# CLI signs WITHOUT hardened runtime on purpose: it loads the
# third-party macFUSE dylib, which a runtime-constrained signature
# rejects in dyld (the appex keeps `--options runtime`).
for marker in 'cargo build --release -p wyrd-cli' 'libiconv.tbd' 'codesign' 'TAURI_ENV_TARGET_TRIPLE'; do
  match "$marker" "$STAGE_CODE" \
    || die "stage-cli: '$marker' missing"
done
match '--options runtime' "$STAGE_CODE" \
  && die "stage-cli: hardened runtime would reject macFUSE"
pass "stage script builds and signs the release CLI"

# 9b. Linkage is covered on both sides of the stage boundary:
# the guard owns the release-dir-relative bundled copy, and the
# stage script checks its own staged copy before signing (the
# staged path is $ROOT-relative, so the guard must never name it
# or an empty release tree would pass).
CHECK_CODE="$(grep -v '^[[:space:]]*#' "$ROOT/packaging/macos/check-no-nix-links.sh")"
match 'Resources/wyrd' "$CHECK_CODE" \
  || die "check: bundled CLI leg missing"
match 'stage/wyrd' "$CHECK_CODE" \
  && die "check: staged path would defeat fail-closed"
match 'otool -L' "$STAGE_CODE" \
  || die "stage-cli: no staged-CLI linkage check"
match '/nix/store' "$STAGE_CODE" \
  || die "stage-cli: linkage check names no Nix store"
pass "linkage is covered on both sides of the stage boundary"

# 10. The decision is recorded in hook-adjacent prose: the packaging
# README names the command, the bundled CLI, the boundary, and the
# team-signed pointer together.
for marker in 'install-dev.sh' 'Resources/wyrd' 'no-group-container' 'team-signed'; do
  grep -Fq "$marker" "$ROOT/packaging/macos/README.md" \
    || die "README: '$marker' not recorded"
done
pass "README records the command, the CLI, and the Finder boundary"

# 11. The suite itself is wired into the native CI shell job.
grep -Fq 'test-dev-install.sh' "$ROOT/.ngit/nix/workflows/default.nix" \
  || die "CI: dev-install suite not wired into shell.suites"
pass "CI wires the dev-install suite"

echo "dev-install: $PASS checks passed"
