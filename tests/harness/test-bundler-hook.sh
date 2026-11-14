#!/usr/bin/env bash
# tests/harness/test-bundler-hook.sh — pin the Tauri bundler hook:
# the stage script builds the appex at the app version, the config
# copies it into Contents/PlugIns before signing, and the app side
# carries nothing. Linux-safe: static assertions only (the real
# stage run and the bundle itself are macOS-only manual evidence).
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
# second — the bundler's mapping direction).
python3 - "$CONF" <<'EOF'
import json, sys
conf = json.load(open(sys.argv[1]))
build = conf.get('build', {})
assert 'stage-appex.sh' in build.get('beforeBundleCommand', ''), build.get('build')
files = conf.get('bundle', {}).get('macOS', {}).get('files', {})
assert files.get('PlugIns/WyrdFileProvider.appex') == \
    '../../packaging/macos/stage/WyrdFileProvider.appex', files
assert 'entitlements' not in conf.get('bundle', {}).get('macOS', {}), \
    'app holds no entitlements (decided)'
EOF
pass "config stages before bundling, copies to PlugIns, no app entitlements"

# 2. The stage script is macOS-only (skips elsewhere, exit 0), builds
# through build-appex.sh, and derives the version from the config —
# never hardcoded, never passed in.
grep -Fq 'uname -s' "$STAGE" || die "stage: no platform guard"
grep -Fq 'exit 0' "$STAGE" || die "stage: no clean skip"
grep -Fq 'fileprovider/build-appex.sh' "$STAGE" \
  || die "stage: does not build through build-appex.sh"
grep -Fq 'tauri.conf.json' "$STAGE" \
  || die "stage: version source is not tauri.conf.json"
grep -Fq 'APPEX_BUNDLE_VERSION' "$STAGE" \
  || die "stage: does not pin CFBundleVersion to the app version"
pass "stage script builds macOS-only at the config version"

# 3. The stage script carries nothing app-side: it never names an app
# entitlements file, an app profile, or the outer bundle — Tauri owns
# all three. The appex profile flows only through build-appex.sh env.
grep -q 'Wyrd.entitlements' "$STAGE" \
  && die "stage: app entitlements have no hook role"
grep -q '\.app/' "$STAGE" \
  && die "stage: hook must not touch the outer bundle"
grep -q 'embedded.provisionprofile' "$STAGE" \
  && die "stage: profile embed belongs to build-appex.sh only"
pass "stage script carries nothing app-side"

# 4. The version rewrite in build-appex.sh lands between the Info.plist
# copy and the signing, so the signature covers the shipped version;
# unset keeps the checked-in value for manual builds.
copy_line=$(grep -n 'cp "\$HERE/Info.plist"' "$BUILD" | head -1 | cut -d: -f1)
rewrite_line=$(grep -n 'Set :CFBundleVersion' "$BUILD" | head -1 | cut -d: -f1)
sign_line=$(grep -n '^codesign ' "$BUILD" | head -1 | cut -d: -f1)
[[ -n "$copy_line" && -n "$rewrite_line" && -n "$sign_line" ]] \
  || die "build-appex.sh: version flow incomplete"
[[ "$copy_line" -lt "$rewrite_line" && "$rewrite_line" -lt "$sign_line" ]] \
  || die "build-appex.sh: rewrite must sit between copy and sign"
grep -Fq 'if [ -n "${APPEX_BUNDLE_VERSION:-}" ]' "$BUILD" \
  || die "build-appex.sh: rewrite is not opt-in"
pass "version rewrite sits between copy and sign"

# 5. The decision is recorded and the follow-on lists point past the
# hook, not at it.
grep -Fq 'stage-appex.sh' "$ROOT/packaging/macos/README.md" \
  || die "README: hook script not recorded"
grep -Fq 'PlugIns/WyrdFileProvider.appex' "$ROOT/packaging/macos/README.md" \
  || die "README: PlugIns destination not recorded"
pass "README records the hook"

echo "bundler-hook: $PASS checks passed"
