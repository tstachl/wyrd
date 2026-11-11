#!/bin/sh
# Build the Wyrd File Provider appex without Xcode: swiftc against
# the macOS SDK, manual bundle assembly, codesign. No .xcodeproj,
# no Xcode IDE, no XcodeGen. macOS only; CI (Linux) does not run
# this — the contract test is the automated leg.
#
# Usage: packaging/macos/fileprovider/build-appex.sh [out-dir]
# Environment: ARCH (default: host arch), SIGN_IDENTITY (default:
# ad-hoc `-`), MIN_MACOS (default: 13.0), TOOLCHAIN (default: the
# active developer directory), SWIFTC (default: resolved from the
# developer directory), SDK (default: the resolved macOS SDK).
# Works in the default devenv shell and under
# `devenv --profile native-apple shell` alike.
set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUT=${1:-"$HERE/build"}
ARCH=${ARCH:-$(uname -m)}
SIGN_IDENTITY=${SIGN_IDENTITY:--}
MIN_MACOS=${MIN_MACOS:-13.0}

# Toolchain discovery is conventional and script-local: the system
# tools are addressed by absolute path, because under the default
# devenv shell Nix shadows them (a Nix DEVELOPER_DIR hijacks the
# /usr/bin shims; a Nix xcbuild xcrun knows no macOS SDK). No
# shell-wide exports, no hardcoded CLT path.
XCODE_SELECT=/usr/bin/xcode-select
XCRUN=/usr/bin/xcrun

# The Nix-injected values must not leak into discovery. Record the
# incoming state first (a leaking outer shell is worth knowing
# about), then unset in this process (script-local; the calling
# shell is untouched) and assert the clean state exactly —
# genuinely unset, not merely empty. A deliberate Xcode selection
# is expressed through TOOLCHAIN, never by exporting DEVELOPER_DIR
# here.
if [ -n "${DEVELOPER_DIR+x}" ] || [ -n "${SDKROOT+x}" ]; then
    echo "note: removing inherited DEVELOPER_DIR/SDKROOT for this build." >&2
fi
unset DEVELOPER_DIR SDKROOT || {
    echo "error: cannot unset DEVELOPER_DIR/SDKROOT (readonly?);" >&2
    echo "cannot establish a clean Apple toolchain environment." >&2
    echo "Enter the Apple-native shell instead:" >&2
    echo "devenv --profile native-apple shell" >&2
    exit 1
}

# Developer directory from the system selector; aborts with usage
# guidance when neither the CLT nor Xcode is installed.
if [ -n "${TOOLCHAIN:-}" ]; then
    DEVDIR=$TOOLCHAIN
else
    DEVDIR=$($XCODE_SELECT -p 2>/dev/null) || {
        echo "error: no Apple developer tools found." >&2
        echo "Install the macOS Command Line Tools (xcode-select" >&2
        echo "--install) or point TOOLCHAIN at a developer directory." >&2
        exit 1
    }
fi
# The compiler and the SDK resolve through xcrun against that
# directory, so both the CLT layout ($DEVDIR/usr/bin) and the
# Xcode layout ($DEVDIR/Toolchains/...) work unchanged. DEVDIR is
# passed as DEVELOPER_DIR to the lookup only (never exported), so
# a TOOLCHAIN override actually selects the toolchain instead of
# silently resolving the active developer directory.
SWIFTC=${SWIFTC:-$(DEVELOPER_DIR=$DEVDIR $XCRUN --sdk macosx --find swiftc 2>/dev/null)} || {
    echo "error: no swiftc for the macOS SDK under $DEVDIR." >&2
    exit 1
}
SDK=${SDK:-$(DEVELOPER_DIR=$DEVDIR $XCRUN --sdk macosx --show-sdk-path 2>/dev/null)} || {
    echo "error: no macOS SDK under $DEVDIR." >&2
    exit 1
}
# Sanity, per the profile contract: the selected compiler runs,
# and the PATH swift is reported so a shadow is visible.
echo "swiftc: $SWIFTC" >&2
echo "swift on PATH: $(command -v swift)" >&2
if [ ! -x "$SWIFTC" ]; then
    echo "error: no Swift toolchain at $SWIFTC." >&2
    echo "Install the macOS Command Line Tools (xcode-select --install)" >&2
    echo "or point TOOLCHAIN at a full Xcode installation." >&2
    exit 1
fi
"$SWIFTC" --version >/dev/null 2>&1 || {
    echo "error: the selected Swift toolchain does not run." >&2
    exit 1
}

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
    -emit-library \
    -module-name WyrdFileProvider \
    -Xlinker -bundle \
    -o "$MACOS/WyrdFileProvider" \
    "$@"

cp "$HERE/Info.plist" "$CONTENTS/Info.plist"

# The development entitlements travel with every build, including
# ad-hoc ones: embedding is harmless, but the App Group container
# only materializes under a team-signed build with provisioning.
# Value and container directory are the literal group id — no team
# prefix, verified live 2026-11-11 — so no substitution happens here.
# Stage 1 probes the team-signed form and records it.
# TEAM_ID is ignored and always has been: warn rather than let an
# old caller believe the group id was rewritten.
if [ -n "${TEAM_ID:-}" ]; then
    echo "warning: TEAM_ID is ignored; the group id signs as written." >&2
fi
ENTITLEMENTS="$HERE/WyrdFileProvider.entitlements"

codesign --force --sign "$SIGN_IDENTITY" \
    --identifier md.enesis.wyrd.fileprovider \
    --entitlements "$ENTITLEMENTS" \
    "$APPEX"

echo "built $APPEX"
codesign -dv "$APPEX" 2>&1 | sed -n '1,4p'
