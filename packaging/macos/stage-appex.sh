#!/bin/sh
# Stage the File Provider appex for the Tauri bundler.
#
# Runs as `build.beforeBundleCommand` (macOS only): builds the appex
# with `fileprovider/build-appex.sh`, pins its CFBundleVersion to the
# app version from `crates/wyrd-desktop/tauri.conf.json`, and leaves
# it at the fixed staging path that `bundle.macOS.files` copies into
# `Contents/PlugIns`. The bundler then signs the outer app shallowly,
# which preserves the appex's own signature (nested-first order).
#
# Exits 0 on non-macOS platforms so Linux bundles are unaffected.
# App side carries nothing: no app entitlements file, no app profile,
# no touch of Wyrd.app here — Tauri owns the outer bundle.
#
# Environment passthrough to build-appex.sh: SIGN_IDENTITY (default
# ad-hoc `-`), PROVISIONING_PROFILE (default unset), ARCH, MIN_MACOS,
# TOOLCHAIN, SWIFTC, SDK. The signing owner pairs SIGN_IDENTITY with
# the same Developer ID Tauri signs the app with
# (`bundle.macOS.signingIdentity` / APPLE_SIGNING_IDENTITY).
set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

# macOS only: the appex never ships on other targets, and the
# bundler must not fail a Linux build for a macOS-only stage.
if [ "$(uname -s)" != "Darwin" ]; then
    echo "note: stage-appex.sh skips non-macOS platform $(uname -s)." >&2
    exit 0
fi

# Single version source: the app version Tauri reads. The staged
# appex CFBundleVersion must equal it (lockstep, no mixed-version
# support case).
TAURI_CONF="$ROOT/crates/wyrd-desktop/tauri.conf.json"
command -v python3 >/dev/null 2>&1 || {
    echo "error: python3 is required to read the app version." >&2
    exit 1
}
APP_VERSION=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$TAURI_CONF")
[ -n "$APP_VERSION" ] || {
    echo "error: no version in $TAURI_CONF." >&2
    exit 1
}

STAGE="$HERE/stage/WyrdFileProvider.appex"
rm -rf "$STAGE"

APPEX_BUNDLE_VERSION="$APP_VERSION" "$HERE/fileprovider/build-appex.sh" "$HERE/stage"

# Fail closed: the bundler must never copy a version-drifted appex.
STAGED_VERSION=$(/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$STAGE/Contents/Info.plist")
if [ "$STAGED_VERSION" != "$APP_VERSION" ]; then
    echo "error: staged appex CFBundleVersion $STAGED_VERSION != app version $APP_VERSION." >&2
    exit 1
fi

echo "staged $STAGE at CFBundleVersion $STAGED_VERSION"
