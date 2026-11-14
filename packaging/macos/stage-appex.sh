#!/bin/sh
# Stage the File Provider appex for the Tauri bundler.
#
# Runs as `build.beforeBundleCommand` (macOS only): builds the appex
# with `fileprovider/build-appex.sh`, pins its `CFBundleVersion` and
# `CFBundleShortVersionString` to the app version from
# `crates/wyrd-desktop/tauri.conf.json`, and leaves it at the fixed
# staging path that `bundle.macOS.files` copies into
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
# (`bundle.macOS.signingIdentity` / APPLE_SIGNING_IDENTITY); a
# team-signed outer app with an ad-hoc appex is refused below.
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

# Architecture lockstep: Tauri exports TAURI_ENV_TARGET_TRIPLE for
# the bundle it builds. A cross-target bundle must not silently
# embed a host-arch appex, and there is no universal appex build —
# per-arch bundles build on their host (the release runbook ships
# per-arch DMGs). Manual runs without Tauri keep the build-appex.sh
# default (ARCH or host arch).
if [ -n "${TAURI_ENV_TARGET_TRIPLE:-}" ]; then
    case "$TAURI_ENV_TARGET_TRIPLE" in
        aarch64-apple-darwin) ARCH=arm64 ;;
        x86_64-apple-darwin) ARCH=x86_64 ;;
        *)
            echo "error: stage-appex.sh cannot satisfy target $TAURI_ENV_TARGET_TRIPLE (native per-arch builds only)." >&2
            exit 1
            ;;
    esac
    export ARCH
fi

# Config source: the app version and the macOS signing identity
# Tauri reads. python3 parses the config on macOS and in CI.
TAURI_CONF="$ROOT/crates/wyrd-desktop/tauri.conf.json"
command -v python3 >/dev/null 2>&1 || {
    echo "error: python3 is required to read the Tauri config." >&2
    exit 1
}

# Identity pairing: the outer app is team-signed when Tauri has a
# real identity — APPLE_SIGNING_IDENTITY in the environment or
# bundle.macOS.signingIdentity in the config. A real outer identity
# with an ad-hoc or different appex identity is the mixed state the
# release runbook forbids — fail closed and name the fix. `-`
# (Tauri's ad-hoc pseudo-identity) and unset both mean ad-hoc, so
# the dev flow (neither set) and explicit ad-hoc pairs pass.
OUTER_IDENTITY="${APPLE_SIGNING_IDENTITY:-}"
if [ -z "$OUTER_IDENTITY" ]; then
    OUTER_IDENTITY=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("bundle",{}).get("macOS",{}).get("signingIdentity",""))' "$TAURI_CONF")
fi
case "$OUTER_IDENTITY" in
    ""|-) ;;
    *)
        if [ "${SIGN_IDENTITY:--}" != "$OUTER_IDENTITY" ]; then
            echo "error: outer app signs as $OUTER_IDENTITY but the appex would sign as ${SIGN_IDENTITY:--}; set SIGN_IDENTITY to the same identity." >&2
            exit 1
        fi
        ;;
esac

# Single version source: the app version Tauri reads. Both staged
# version keys must equal it (lockstep, no mixed-version support
# case).
APP_VERSION=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$TAURI_CONF")
[ -n "$APP_VERSION" ] || {
    echo "error: no version in $TAURI_CONF." >&2
    exit 1
}

STAGE="$HERE/stage/WyrdFileProvider.appex"
rm -rf "$STAGE"

APPEX_BUNDLE_VERSION="$APP_VERSION" "$HERE/fileprovider/build-appex.sh" "$HERE/stage"

# Fail closed: the bundler must never copy a version-drifted appex.
for key in CFBundleVersion CFBundleShortVersionString; do
    STAGED_VERSION=$(/usr/libexec/PlistBuddy -c "Print :$key" "$STAGE/Contents/Info.plist")
    if [ "$STAGED_VERSION" != "$APP_VERSION" ]; then
        echo "error: staged appex $key $STAGED_VERSION != app version $APP_VERSION." >&2
        exit 1
    fi
done

echo "staged $STAGE at version $APP_VERSION"
