#!/bin/sh
# Build the File Provider domain registrar as a signed helper
# executable.
#
# macOS only. Wyrd.app invokes this helper to call the File Provider
# domain API; it is signed with the app's bundle identifier so the
# registration associates with the containing app. No .xcodeproj, no
# IDE, no XcodeGen — the same script-driven swiftc shape as
# ../build-appex.sh.
#
# Usage: packaging/macos/domain-registrar/build-registrar.sh [out-dir]
# Environment: ARCH (default host arch), SIGN_IDENTITY (default `-`),
# MIN_MACOS (default 13.0), TOOLCHAIN, SWIFTC, SDK.
set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
OUT=${1:-"$HERE/build"}
ARCH=${ARCH:-$(uname -m)}
SIGN_IDENTITY=${SIGN_IDENTITY:--}
MIN_MACOS=${MIN_MACOS:-13.0}

if [ "$(uname -s)" != "Darwin" ]; then
    echo "error: build-registrar.sh builds a macOS dylib only." >&2
    exit 1
fi

# The devenv shell injects a Nix SDK that hijacks the /usr/bin shims.
# Undo it process-locally and discover the Apple toolchain the same
# way build-appex.sh does: absolute system tools, unset injected
# variables, TOOLCHAIN as the deliberate override (never exported).
if [ -n "${DEVELOPER_DIR+x}" ] || [ -n "${SDKROOT+x}" ]; then
    echo "note: removing inherited DEVELOPER_DIR/SDKROOT for this build." >&2
fi
unset DEVELOPER_DIR SDKROOT || {
    echo "error: cannot unset DEVELOPER_DIR/SDKROOT (readonly?)." >&2
    exit 1
}
XCODE_SELECT=/usr/bin/xcode-select
XCRUN=/usr/bin/xcrun
if [ -n "${TOOLCHAIN:-}" ]; then
    DEVDIR=$TOOLCHAIN
else
    DEVDIR=$($XCODE_SELECT -p 2>/dev/null) || {
        echo "error: no Apple developer tools found." >&2
        exit 1
    }
fi
SWIFTC=${SWIFTC:-$(DEVELOPER_DIR=$DEVDIR $XCRUN --sdk macosx --find swiftc 2>/dev/null)} || {
    echo "error: no swiftc for the macOS SDK under $DEVDIR." >&2
    exit 1
}
SDK=${SDK:-$(DEVELOPER_DIR=$DEVDIR $XCRUN --sdk macosx --show-sdk-path 2>/dev/null)} || {
    echo "error: no macOS SDK under $DEVDIR." >&2
    exit 1
}
if [ ! -x "$SWIFTC" ]; then
    echo "error: no Swift toolchain at $SWIFTC." >&2
    exit 1
fi

HELPER="$OUT/WyrdDomainRegistrar"
mkdir -p "$OUT"
rm -f "$HELPER"

"$SWIFTC" \
    -target "$ARCH-apple-macos$MIN_MACOS" \
    -sdk "$SDK" \
    -O \
    -o "$HELPER" \
    "$HERE/main.swift"

# The helper must present the containing app's identity: File Provider
# domain registration is associated with the calling app, and a
# different signing identifier would register against a different
# app. An ad-hoc install signs ad-hoc with the same identifier.
ENTITLEMENTS="$ROOT/packaging/macos/Wyrd.app.entitlements"
codesign --force --sign "$SIGN_IDENTITY" \
    --identifier md.enesis.wyrd \
    --entitlements "$ENTITLEMENTS" \
    --options runtime \
    "$HELPER"

echo "built $HELPER"
codesign -dv "$HELPER" 2>&1 | sed -n '1,4p'
