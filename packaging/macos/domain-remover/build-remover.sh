#!/bin/sh
# Build the supervised File Provider domain remover as a signed helper.
#
# macOS only. The uninstall script invokes this helper to remove every
# domain of this provider. It is signed with the containing app's bundle
# identifier so NSFileProviderManager acts on the same provider. It is
# never embedded in Wyrd.app and never called by the product registrar.
#
# Usage: packaging/macos/domain-remover/build-remover.sh [out-dir]
# Environment: ARCH (default host arch), SIGN_IDENTITY (default `-`),
# MIN_MACOS (default 13.0), TOOLCHAIN, SWIFTC, SDK, WYRD_APP_GROUP.
set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
. "$ROOT/packaging/macos/app-group.sh"
APP_GROUP=$(wyrd_app_group)
echo "app group: $APP_GROUP" >&2
OUT=${1:-"$HERE/build"}
ARCH=${ARCH:-$(uname -m)}
SIGN_IDENTITY=${SIGN_IDENTITY:--}
MIN_MACOS=${MIN_MACOS:-13.0}

if [ "$(uname -s)" != "Darwin" ]; then
    echo "error: build-remover.sh builds a macOS executable only." >&2
    exit 1
fi

# Undo the devenv SDK injection process-locally, as the other Swift
# packaging scripts do, so /usr/bin/xcrun finds the Apple toolchain.
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

HELPER="$OUT/WyrdDomainRemover"
mkdir -p "$OUT"
rm -f "$HELPER"

"$SWIFTC" \
    -target "$ARCH-apple-macos$MIN_MACOS" \
    -sdk "$SDK" \
    -O \
    -o "$HELPER" \
    "$HERE/main.swift"

# The helper must present the app's identity: File Provider domains are
# associated with the containing app's bundle identifier.
ENTITLEMENTS="$OUT/WyrdDomainRemover.entitlements"
wyrd_render_app_group "$ROOT/packaging/macos/Wyrd.app.entitlements" \
    "$WYRD_APP_GROUP_DEFAULT" "$ENTITLEMENTS"
codesign --force --sign "$SIGN_IDENTITY" \
    --identifier md.enesis.wyrd \
    --entitlements "$ENTITLEMENTS" \
    --options runtime \
    "$HELPER"

echo "built $HELPER"
codesign -dv "$HELPER" 2>&1 | sed -n '1,4p'
