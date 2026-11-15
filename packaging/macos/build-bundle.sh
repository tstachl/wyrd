#!/bin/sh
# Build the Tauri bundle with the macOS SDK link-order pin.
#
# Usage: packaging/macos/build-bundle.sh [--bundles app ...]
# Environment passthrough to the bundler hook and build-appex.sh:
# SIGN_IDENTITY, PROVISIONING_PROFILE, ARCH, MIN_MACOS, TOOLCHAIN,
# SWIFTC, SDK, APPEX_BUNDLE_VERSION. With no arguments the wrapper
# builds `--bundles app` (the release runbook shape); any arguments
# replace the default and forward to `cargo tauri build` verbatim.
#
# Background: under the default devenv shell the Nix cc wrapper puts
# `-L/nix/store/...-libiconv.../lib` on the link line (via libc's
# `link(name = "iconv")`), so the Rust binary records a Nix-store
# libiconv. A team-signed bundle carries hardened runtime, whose
# library validation rejects that foreign-team dylib at launch
# (`different Team IDs`, dead before main). Passing
# `-L<MacOSX.sdk>/usr/lib` makes the SDK libiconv.tbd
# (`/usr/lib/libiconv.2.dylib`, same ABI 7.0.0) win with no source
# or entitlement changes. The pin is Darwin-only: elsewhere the
# wrapper forwards the build untouched.
set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

# Link-order pin (Darwin only). Discovery mirrors
# fileprovider/build-appex.sh: system tools by absolute path, the
# Nix-injected values unset process-locally so the /usr/bin shims
# resolve the Apple toolchain, TOOLCHAIN as the deliberate override
# (never an exported DEVELOPER_DIR here).
if [ "$(uname -s)" = "Darwin" ]; then
    if [ -n "${DEVELOPER_DIR+x}" ] || [ -n "${SDKROOT+x}" ]; then
        echo "note: removing inherited DEVELOPER_DIR/SDKROOT for SDK discovery." >&2
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
            echo "Install the macOS Command Line Tools (xcode-select --install)" >&2
            echo "or point TOOLCHAIN at a developer directory." >&2
            exit 1
        }
    fi
    if [ -n "${SDK:-}" ]; then
        DISCOVERED_SDK=$SDK
    else
        DISCOVERED_SDK=$(DEVELOPER_DIR=$DEVDIR $XCRUN --sdk macosx --show-sdk-path 2>/dev/null) || {
            echo "error: no macOS SDK under $DEVDIR." >&2
            exit 1
        }
    fi
    if [ ! -f "$DISCOVERED_SDK/usr/lib/libiconv.tbd" ]; then
        echo "error: SDK has no usr/lib/libiconv.tbd: $DISCOVERED_SDK" >&2
        exit 1
    fi
    RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-L$DISCOVERED_SDK/usr/lib"
    export RUSTFLAGS
    echo "note: macOS link-order pin: -L$DISCOVERED_SDK/usr/lib" >&2
fi

DESKTOP="$ROOT/crates/wyrd-desktop"
if [ "$#" -eq 0 ]; then
    set -- --bundles app
fi
cd "$DESKTOP"
cargo tauri build "$@"

# No-Nix-links guard (Darwin only): the shipped binary must link no
# /nix/store path, or hardened runtime fails library validation at
# launch. otool absent means not a macOS bundle host — skip.
if [ "$(uname -s)" = "Darwin" ]; then
    if ! command -v otool >/dev/null 2>&1; then
        echo "note: otool absent, skipping no-Nix-links guard." >&2
        exit 0
    fi
    BUNDLE_BIN="$ROOT/target/release/bundle/macos/Wyrd.app/Contents/MacOS/wyrd-desktop"
    STANDALONE_BIN="$ROOT/target/release/wyrd-desktop"
    FOUND=0
    for BIN in "$BUNDLE_BIN" "$STANDALONE_BIN"; do
        if [ -f "$BIN" ]; then
            if otool -L "$BIN" | grep -q '/nix/store'; then
                echo "error: $BIN links /nix/store paths:" >&2
                otool -L "$BIN" | grep '/nix/store' >&2
                FOUND=1
            fi
        fi
    done
    if [ "$FOUND" -ne 0 ]; then
        echo "error: no-Nix-links guard failed; see paths above." >&2
        exit 1
    fi
    echo "note: no-Nix-links guard passed." >&2
fi
