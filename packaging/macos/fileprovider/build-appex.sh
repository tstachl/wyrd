#!/bin/sh
# Build the Wyrd File Provider appex without Xcode: swiftc against
# the macOS SDK, manual bundle assembly, codesign. No .xcodeproj,
# no Xcode IDE, no XcodeGen. macOS only; CI (Linux) does not run
# this — the contract test is the automated leg.
#
# Usage: packaging/macos/fileprovider/build-appex.sh [out-dir]
# Environment: ARCH (default: host arch), SIGN_IDENTITY (default:
# ad-hoc `-`), MIN_MACOS (default: 13.0).
set -eu

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

APPEX=$OUT/WyrdFileProvider.appex
CONTENTS=$APPEX/Contents
MACOS=$CONTENTS/MacOS

rm -rf "$APPEX"
mkdir -p "$MACOS"

"$SWIFTC" \
    -target "$ARCH-apple-macos$MIN_MACOS" \
    -sdk "$SDK" \
    -O \
    -module-name WyrdFileProvider \
    -o "$MACOS/WyrdFileProvider" \
    "$HERE"/Sources/*.swift

cp "$HERE/Info.plist" "$CONTENTS/Info.plist"

codesign --force --sign "$SIGN_IDENTITY" \
    --identifier md.enesis.wyrd.fileprovider \
    "$APPEX"

echo "built $APPEX"
codesign -dv "$APPEX" 2>&1 | head -n 4
