#!/bin/sh
# Build the Tauri bundle with the macOS SDK link-order pin.
#
# Also exports SDKROOT=<discovered SDK> around the cargo
# invocations below: rustc honors it directly and skips its bare
# xcrun query, which under devenv resolves to the Nix shadow.
#
# Usage: packaging/macos/build-bundle.sh [--bundles app ...]
# Environment passthrough to the bundler hook and build-appex.sh:
# SIGN_IDENTITY, PROVISIONING_PROFILE (the appex), APP_PROVISIONING_PROFILE
# (the app), ARCH, MIN_MACOS, TOOLCHAIN, SWIFTC, SDK, APPEX_BUNDLE_VERSION,
# WYRD_APP_GROUP (default: the value in packaging/macos/app-group.sh),
# WYRD_FINDER_CAPABILITY (adhoc | identity | profile; when unset, inferred
# from the signing inputs so a direct build matches its own signature).
# With no arguments the wrapper
# builds `--bundles app` (the release runbook shape); any arguments
# replace the default and forward to `cargo tauri build` verbatim,
# with a `--target <triple>` argument (or CARGO_BUILD_TARGET) moving
# the release dir the guard inspects accordingly. Non-release shapes
# (`--debug`, `--profile`, `--target-dir`) are refused: the guard
# tracks the release tree only.
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
. "$HERE/app-group.sh"
. "$HERE/sign-identity.sh"

# Stage the Finder capability fact for every build path. The bundler
# copies it into Contents/Resources before the outer sign, so a direct
# wrapper run cannot inherit a marker left by a different install mode.
# The shared helper resolves it from the caller's declaration or, when
# none is given, from the signing inputs of this very build.
CAPABILITY=$(wyrd_finder_capability) || {
    echo "error: WYRD_FINDER_CAPABILITY must be adhoc, identity or profile." >&2
    exit 1
}
mkdir -p "$HERE/stage"
printf '%s\n' "$CAPABILITY" > "$HERE/stage/finder-capability.txt"

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
            echo "error: SDK path contains whitespace," >&2
            echo "which RUSTFLAGS cannot carry: $DISCOVERED_SDK" >&2
            echo "Point SDK at a whitespace-free path." >&2
            exit 1
            ;;
    esac
    RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-L$DISCOVERED_SDK/usr/lib"
    export RUSTFLAGS
    # rustc discovers the macOS SDK with its own bare `xcrun` query,
    # which under devenv resolves to the Nix shadow and warns. It
    # honors SDKROOT directly and skips that query, so pin the
    # discovered SDK for the cargo invocations below.
    SDKROOT="$DISCOVERED_SDK"
    export SDKROOT
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
# and covers the appex and co-installed CLI binaries too. The release dir defaults to a
# CARGO_TARGET_DIR-aware target/release, moves with a forwarded
# --target <triple> or CARGO_BUILD_TARGET, and refuses non-release
# shapes outright; anything beyond these passes its release dir to
# check-no-nix-links.sh explicitly. otool absent means not a macOS
# bundle host — the check skips itself.
if [ "$(uname -s)" = "Darwin" ]; then
    # A --target <triple> build moves the artifacts; the guard must
    # follow or it inspects stale host output (or nothing). Scan the
    # forwarded args for it, with CARGO_BUILD_TARGET as the env
    # fallback; non-release shapes are refused above the guard, and
    # anything beyond these passes its release dir to the check
    # explicitly.
    RELEASE_BASE="${CARGO_TARGET_DIR:-$ROOT/target}"
    TRIPLE=""
    NEXT_IS_TARGET=0
    NEXT_IS_PROFILE=0
    for ARG in "$@"; do
        if [ "$NEXT_IS_TARGET" -eq 1 ]; then
            TRIPLE="$ARG"
            NEXT_IS_TARGET=0
        elif [ "$NEXT_IS_PROFILE" -eq 1 ]; then
            echo "error: --profile $ARG builds outside release; refusing a shape the guard does not track." >&2
            exit 1
        else
            case "$ARG" in
                --target) NEXT_IS_TARGET=1 ;;
                --target=*) TRIPLE="${ARG#--target=}" ;;
                --profile) NEXT_IS_PROFILE=1 ;;
                --profile=*) echo "error: $ARG builds outside release; refusing a shape the guard does not track." >&2; exit 1 ;;
                --debug) echo "error: --debug builds to debug, not release; refusing a shape the guard does not track." >&2; exit 1 ;;
                --target-dir|--target-dir=*) echo "error: $ARG moves the target dir; refusing a shape the guard does not track." >&2; exit 1 ;;
            esac
        fi
    done
    if [ "$NEXT_IS_TARGET" -eq 1 ]; then
        echo "error: --target without a triple; refusing a build the guard cannot locate." >&2
        exit 1
    fi
    if [ -z "$TRIPLE" ] && [ -n "${CARGO_BUILD_TARGET:-}" ]; then
        TRIPLE="$CARGO_BUILD_TARGET"
    fi
    if [ -n "$TRIPLE" ]; then
        RELEASE_DIR="$RELEASE_BASE/$TRIPLE/release"
    else
        RELEASE_DIR="$RELEASE_BASE/release"
    fi
    # The app's provisioning profile is embedded before the reseal:
    # Tauri has no config field for it, and a post-sign copy would
    # invalidate the outer signature. The profile authorizes the App
    # Group entitlement Wyrd.app needs to materialize its container.
    if [ -d "$RELEASE_DIR/bundle/macos/Wyrd.app" ] \
        && [ -n "${APP_PROVISIONING_PROFILE:-}" ]; then
        [ -f "$APP_PROVISIONING_PROFILE" ] || {
            echo "error: APP_PROVISIONING_PROFILE names no file: $APP_PROVISIONING_PROFILE" >&2
            exit 1
        }
        cp "$APP_PROVISIONING_PROFILE" \
            "$RELEASE_DIR/bundle/macos/Wyrd.app/Contents/embedded.provisionprofile"
    fi
    # Reseal: the bundler leaves the outer app with a linker-only
    # signature, so the staged files it copied
    # (`Contents/Resources/wyrd` beside the appex) are unsealed and
    # a deep-strict verify fails closed (observed 2026-11-15). A
    # shallow re-sign seals the current tree while preserving the
    # nested appex signature (never `--deep`, which would re-sign
    # the appex without its entitlements) — the nested-first order
    # the runbook documents. Identity pairs with the hook:
    # stage-appex.sh refuses a mixed pair except in the narrow
    # stable-identity dev mode, so the default ad-hoc `-` here means
    # an ad-hoc outer app, and a set SIGN_IDENTITY is the same
    # Developer ID Tauri used. A real
    # identity seals with a secure timestamp (notarization
    # requires it); ad-hoc has no timestamp authority to reach.
    if [ -d "$RELEASE_DIR/bundle/macos/Wyrd.app" ]; then
        # Carry the app entitlements explicitly: a resign without
        # them silently drops the App Group grant Tauri embedded.
        # Render WYRD_APP_GROUP into a build-local copy — the
        # checked-in file holds the default so Tauri's own sign stays
        # valid, and this re-sign makes the override effective.
        APP_ENTITLEMENTS_SRC="$ROOT/packaging/macos/Wyrd.app.entitlements"
        [ -f "$APP_ENTITLEMENTS_SRC" ] || {
            echo "error: app entitlements not found: $APP_ENTITLEMENTS_SRC" >&2
            exit 1
        }
        APP_ENTITLEMENTS="$RELEASE_DIR/wyrd-app.entitlements"
        wyrd_render_app_group "$APP_ENTITLEMENTS_SRC" "$WYRD_APP_GROUP_DEFAULT" "$APP_ENTITLEMENTS"
        if wyrd_is_real_identity "${SIGN_IDENTITY:-}"; then
            codesign --force --sign "$SIGN_IDENTITY" --options runtime --timestamp \
                --entitlements "$APP_ENTITLEMENTS" \
                "$RELEASE_DIR/bundle/macos/Wyrd.app"
        else
            codesign --force --sign "${SIGN_IDENTITY:-$WYRD_ADHOC_IDENTITY}" --options runtime \
                --entitlements "$APP_ENTITLEMENTS" \
                "$RELEASE_DIR/bundle/macos/Wyrd.app"
        fi
    fi
    "$HERE/check-no-nix-links.sh" "$ROOT" "$RELEASE_DIR"
fi
