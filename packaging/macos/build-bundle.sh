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
    case "$DISCOVERED_SDK" in
        *[[:space:]]*)
            echo "error: SDK path contains whitespace, which RUSTFLAGS cannot carry: $DISCOVERED_SDK" >&2
            echo "Point SDK at a whitespace-free path." >&2
            exit 1
            ;;
    esac
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

# No-Nix-links guard (Darwin only): delegate to
# check-no-nix-links.sh, which fails closed when it inspects nothing
# and covers the appex binary too. otool absent means not a macOS
# bundle host — the check skips itself.
if [ "$(uname -s)" = "Darwin" ]; then
    "$HERE/check-no-nix-links.sh" "$ROOT"
fi
