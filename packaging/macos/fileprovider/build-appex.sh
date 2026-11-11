#!/bin/sh
# Build the Wyrd File Provider appex without Xcode: swiftc against
# the macOS SDK, manual bundle assembly, codesign. No .xcodeproj,
# no Xcode IDE, no XcodeGen. macOS only; CI (Linux) does not run
# this — the contract test is the automated leg.
#
# Usage: packaging/macos/fileprovider/build-appex.sh [out-dir]
# Environment: ARCH (default: host arch), SIGN_IDENTITY (default:
# ad-hoc `-`), MIN_MACOS (default: 13.0), TEAM_ID (default: unset;
# when set, the App Group id is rewritten with the team prefix).
set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUT=${1:-"$HERE/build"}
ARCH=${ARCH:-$(uname -m)}
SIGN_IDENTITY=${SIGN_IDENTITY:--}
MIN_MACOS=${MIN_MACOS:-13.0}

# The devenv shell exports a Nix DEVELOPER_DIR that hijacks the
# /usr/bin shims, so the CLT toolchain is addressed directly. The
# CLT ships swiftc, the macOS SDK (FileProvider included), and
# codesign: no full Xcode install, no .xcodeproj, no XcodeGen.
TOOLCHAIN=${TOOLCHAIN:-/Library/Developer/CommandLineTools}
SWIFTC=$TOOLCHAIN/usr/bin/swiftc
SDK=${SDK:-$TOOLCHAIN/SDKs/MacOSX.sdk}
unset DEVELOPER_DIR

if [ ! -x "$SWIFTC" ]; then
    echo "error: no Swift toolchain at $SWIFTC." >&2
    echo "Install the macOS Command Line Tools (xcode-select --install)" >&2
    echo "or point TOOLCHAIN at a full Xcode installation." >&2
    exit 1
fi

APPEX=$OUT/WyrdFileProvider.appex
CONTENTS=$APPEX/Contents
MACOS=$CONTENTS/MacOS

rm -rf "$APPEX"
mkdir -p "$MACOS"

# The contract test walks Sources recursively; the build compiles
# the same set, so a nested file is never checked-but-omitted.
# Capture first: an unquoted $(find …) would split on whitespace
# and discard find's status, silently compiling a partial list.
sources=$(find "$HERE/Sources" -name '*.swift' -print) || {
    echo "error: cannot enumerate $HERE/Sources" >&2
    exit 1
}
IFS='
'
set -f
set -- $sources
unset IFS
set +f
if [ "$#" -eq 0 ]; then
    echo "error: no Swift sources under $HERE/Sources" >&2
    exit 1
fi

"$SWIFTC" \
    -target "$ARCH-apple-macos$MIN_MACOS" \
    -sdk "$SDK" \
    -O \
    -module-name WyrdFileProvider \
    -o "$MACOS/WyrdFileProvider" \
    "$@"

cp "$HERE/Info.plist" "$CONTENTS/Info.plist"

# The development entitlements travel with every build, including
# ad-hoc ones: embedding is harmless, but the App Group container
# only materializes under a team-signed build with provisioning.
# TEAM_ID names that team: when set, the group id is rewritten to
# <TEAM_ID>.group.md.enesis.wyrd and the container becomes
# ~/Library/Group Containers/<TEAM_ID>.group.md.enesis.wyrd/.
# Without it the checked-in file signs as-is and no container
# exists. Stage 1 probes the team-signed form and records it.
ENTITLEMENTS_SRC="$HERE/WyrdFileProvider.entitlements"
if [ -n "${TEAM_ID:-}" ]; then
    if [ "$SIGN_IDENTITY" = "-" ]; then
        echo "warning: TEAM_ID is set but SIGN_IDENTITY is ad-hoc;" >&2
        echo "the group container still will not exist." >&2
    fi
    ENTITLEMENTS_BUILD="$OUT/WyrdFileProvider.entitlements"
    sed "s/group\\.md\\.enesis\\.wyrd/$TEAM_ID.group.md.enesis.wyrd/" \
        "$ENTITLEMENTS_SRC" > "$ENTITLEMENTS_BUILD"
    ENTITLEMENTS=$ENTITLEMENTS_BUILD
else
    ENTITLEMENTS=$ENTITLEMENTS_SRC
fi

codesign --force --sign "$SIGN_IDENTITY" \
    --identifier md.enesis.wyrd.fileprovider \
    --entitlements "$ENTITLEMENTS" \
    "$APPEX"

echo "built $APPEX"
codesign -dv "$APPEX" 2>&1 | sed -n '1,4p'
