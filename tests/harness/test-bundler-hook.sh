#!/usr/bin/env bash
# tests/harness/test-bundler-hook.sh — pin the Tauri bundler hook:
# the stage script builds the appex at the app version and arch, the
# config copies it into Contents/PlugIns before signing, hardened
# runtime and identities pair correctly, and the app side carries
# nothing. Linux-safe: static assertions only (the real stage run
# and the bundle itself are macOS-only manual evidence).
#
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-bundler-hook.sh`. (Wired into the CI
# shell-tests job alongside the other stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

CONF="$ROOT/crates/wyrd-desktop/tauri.conf.json"
STAGE="$ROOT/packaging/macos/stage-appex.sh"
BUILD="$ROOT/packaging/macos/fileprovider/build-appex.sh"

# 1. The config runs the stage script before bundling and copies the
# staged appex into Contents/PlugIns (destination first, source
# second — the bundler's mapping direction). The outer app carries
# the App Group entitlement (earlier "no entitlements" decision was
# reopened; see packaging/macos/README.md).
python3 - "$CONF" <<'EOF'
import json, sys
conf = json.load(open(sys.argv[1]))
build = conf.get('build', {})
assert 'stage-appex.sh' in build.get('beforeBundleCommand', ''), build.get('build')
files = conf.get('bundle', {}).get('macOS', {}).get('files', {})
assert files.get('PlugIns/WyrdFileProvider.appex') == \
    '../../packaging/macos/stage/WyrdFileProvider.appex', files
assert conf.get('bundle', {}).get('macOS', {}).get('entitlements') == \
    '../../packaging/macos/Wyrd.app.entitlements', \
    'app must declare the App Group entitlements file'
EOF
pass "config stages before bundling, copies to PlugIns, wires app entitlements"

# 2. The stage script is macOS-only (skips elsewhere, exit 0), builds
# through build-appex.sh with the config version stamped in the
# environment — the exact assignment, not a comment mention.
grep -Fq 'if [ "$(uname -s)" != "Darwin" ]' "$STAGE" \
  || die "stage: no macOS platform guard"
grep -Fq 'APPEX_BUNDLE_VERSION="$APP_VERSION" "$HERE/fileprovider/build-appex.sh"' "$STAGE" \
  || die "stage: version is not passed to build-appex.sh in the environment"
pass "stage script builds macOS-only at the config version"

# 3. The stage script maps Tauri's target triple to the appex arch
# and refuses targets it cannot satisfy (no silent host-arch embed,
# no universal appex).
grep -Fq 'TAURI_ENV_TARGET_TRIPLE' "$STAGE" \
  || die "stage: no target-triple mapping"
grep -Fq 'aarch64-apple-darwin) ARCH=arm64' "$STAGE" \
  || die "stage: no aarch64 mapping"
grep -Fq 'x86_64-apple-darwin) ARCH=x86_64' "$STAGE" \
  || die "stage: no x86_64 mapping"
pass "stage script pins the appex arch to the bundle target"

# 4. Identity pairing fails closed: a real outer identity —
# APPLE_SIGNING_IDENTITY in the environment or
# bundle.macOS.signingIdentity in the config — with an ad-hoc or
# different appex identity is refused with the fix named. `-`
# (Tauri's ad-hoc pseudo-identity) and unset both mean ad-hoc.
grep -Fq 'APPLE_SIGNING_IDENTITY' "$STAGE" \
  || die "stage: no outer-identity pairing"
grep -Fq 'signingIdentity' "$STAGE" \
  || die "stage: config signing identity is not read"
grep -Fq '""|-)' "$STAGE" \
  || die "stage: ad-hoc - is not treated as ad-hoc"
grep -Fq 'set SIGN_IDENTITY to the same identity' "$STAGE" \
  || die "stage: pairing refusal names no fix"
pass "stage script refuses a mismatched appex identity"

# 5. The stage script carries nothing app-side: it never names an app
# entitlements file, an app profile, or the outer bundle — Tauri owns
# all three. The appex profile flows only through build-appex.sh env.
grep -q 'Wyrd.entitlements' "$STAGE" \
  && die "stage: app entitlements have no hook role"
grep -q '\.app/' "$STAGE" \
  && die "stage: hook must not touch the outer bundle"
grep -q 'embedded.provisionprofile' "$STAGE" \
  && die "stage: profile embed belongs to build-appex.sh only"
pass "stage script carries nothing app-side"

# 6. Hardened runtime is on the appex signature: Tauri's shallow
# outer sign cannot repair a nested signature that lacks it, and a
# Developer ID build without it is not notarizable.
grep -Fq -- '--options runtime' "$BUILD" \
  || die "build-appex.sh: appex signature lacks hardened runtime"
pass "appex signature carries hardened runtime"

# 7. Both version keys land between the Info.plist copy and the
# signing, so the signature covers the shipped versions; unset keeps
# the checked-in values for manual builds.
copy_line=$(grep -n 'cp "\$HERE/Info.plist"' "$BUILD" | head -1 | cut -d: -f1)
rewrite_line=$(grep -n 'Set :CFBundleShortVersionString' "$BUILD" | head -1 | cut -d: -f1)
sign_line=$(grep -n '^codesign ' "$BUILD" | head -1 | cut -d: -f1)
[[ -n "$copy_line" && -n "$rewrite_line" && -n "$sign_line" ]] \
  || die "build-appex.sh: version flow incomplete"
[[ "$copy_line" -lt "$rewrite_line" && "$rewrite_line" -lt "$sign_line" ]] \
  || die "build-appex.sh: rewrite must sit between copy and sign"
grep -Fq 'Set :CFBundleVersion' "$BUILD" \
  || die "build-appex.sh: CFBundleVersion is not rewritten"
grep -Fq 'if [ -n "${APPEX_BUNDLE_VERSION:-}" ]' "$BUILD" \
  || die "build-appex.sh: rewrite is not opt-in"
pass "both version keys move between copy and sign"

# 8. The decision is recorded in hook-specific prose (not the
# pre-existing tree block): the hook script, the
# `beforeBundleCommand` wiring, and the shallow-signing order.
grep -Fq 'stage-appex.sh' "$ROOT/packaging/macos/README.md" \
  || die "README: hook script not recorded"
grep -Fq 'beforeBundleCommand' "$ROOT/packaging/macos/README.md" \
  || die "README: hook wiring not recorded"
grep -Fq 'never `--deep`' "$ROOT/packaging/macos/README.md" \
  || die "README: shallow outer sign not recorded"
pass "README records the hook"

# 9. The operative release instruction carries the same prohibition:
# the runbook signs nested-first and shallowly, and the only
# `--deep` left is the verification line (signing with it would
# re-sign the nested appex without its entitlements).
RUNBOOK="$ROOT/.ngit/release.yaml"
grep -Fq 'never `--deep`' "$RUNBOOK" \
  || die "runbook: shallow-signing prohibition missing"
[ "$(grep -c -- '--deep' "$RUNBOOK")" = "2" ] \
  || die "runbook: --deep appears outside the prohibition and verify lines"
grep -- '--deep' "$RUNBOOK" | grep -Fq 'codesign --verify --deep' \
  || die "runbook: the --deep line is not the verify line"
pass "runbook signs nested-first and shallowly"

echo "bundler-hook: $PASS checks passed"
