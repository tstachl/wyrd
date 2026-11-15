#!/bin/sh
# Developer install for v0.4 (ad-hoc, no fee).
#
# Usage: packaging/macos/install-dev.sh [--dest <path>] [--no-open] [-- <build-bundle args>]
#   --dest <path>   install location (default: /Applications/Wyrd.app when
#                   writable, else ~/Applications/Wyrd.app; DEST env overrides)
#   --no-open       build and install without launching the app
#   remaining args  forwarded verbatim to packaging/macos/build-bundle.sh
#                   (e.g. --target aarch64-apple-darwin)
#
# What it does: builds Wyrd.app ad-hoc through build-bundle.sh (the
# existing bundler hook already stages the appex, pairs identities,
# and pins versions), verifies the signature, and installs the bundle
# to /Applications (or ~/Applications) ready to launch. No signing
# assets required: Xcode/CLT plus the Rust and web toolchains only.
#
# Finder boundary (honest, not a bug): an ad-hoc appex logs
# no-group-container and stays inert — the App Group container only
# materializes under a team-signed build with provisioning. The UI,
# owner, registry, and tray work ad-hoc; Finder integration stays a
# separately team-gated path (packaging/macos/README.md, "Developer
# install" plus "Production entitlements and provisioning").
set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

# macOS only: the .app bundle never ships on other targets.
if [ "$(uname -s)" != "Darwin" ]; then
    echo "error: install-dev.sh builds Wyrd.app on macOS only (this host: $(uname -s))." >&2
    exit 1
fi

# Prerequisites, named with the fix. TOOLCHAIN/SDK/SIGN_IDENTITY pass
# through to build-bundle.sh and the stage hook; nothing here sets them.
if ! /usr/bin/xcode-select -p >/dev/null 2>&1; then
    echo "error: no Apple developer tools found." >&2
    echo "Install the macOS Command Line Tools (xcode-select --install)." >&2
    exit 1
fi
command -v cargo >/dev/null 2>&1 || {
    echo "error: cargo is not on PATH (enter the devenv shell first)." >&2
    exit 1
}
command -v bun >/dev/null 2>&1 || {
    echo "error: bun is not on PATH (the desktop build runs 'bun run build')." >&2
    exit 1
}
command -v python3 >/dev/null 2>&1 || {
    echo "error: python3 is required to read the Tauri config." >&2
    exit 1
}

# Argument split: our flags first, then `--`, then everything else
# forwards verbatim to build-bundle.sh (triple handling,
# release-dir tracking, and the no-Nix-links guard stay in that one
# place). No interleaving: `install-dev.sh --dest … --no-open --
# --target <triple>`.
DEST="${DEST:-}"
OPEN_APP=1
while [ "$#" -gt 0 ]; do
    case "$1" in
        --dest)
            [ -n "${2:-}" ] || { echo "error: --dest needs a path." >&2; exit 1; }
            DEST="$2"
            shift 2
            ;;
        --dest=*)
            DEST="${1#--dest=}"
            shift
            ;;
        --no-open)
            OPEN_APP=0
            shift
            ;;
        --help|-h)
            sed -n '2,20p' "$0"
            exit 0
            ;;
        --)
            shift
            break
            ;;
        --dest*|--no-open|--help)
            echo "error: malformed install flag: $1" >&2
            exit 1
            ;;
        *)
            break
            ;;
    esac
done

# Default destination: system Applications when writable, else the
# user's Applications (no sudo is ever attempted here).
if [ -z "$DEST" ]; then
    if [ -w /Applications ]; then
        DEST="/Applications/Wyrd.app"
    else
        DEST="$HOME/Applications/Wyrd.app"
    fi
fi
case "$DEST" in
    *.app) ;;
    *) echo "error: --dest must name a .app bundle path: $DEST" >&2; exit 1 ;;
esac

# Build through the wrapper (link-order pin plus no-Nix-links guard).
"$HERE/build-bundle.sh" "$@"

# Locate the built bundle: same release-dir rule as the wrapper
# (CARGO_TARGET_DIR-aware, --target/CARGO_BUILD_TARGET triple-aware).
RELEASE_BASE="${CARGO_TARGET_DIR:-$ROOT/target}"
TRIPLE=""
NEXT_IS_TARGET=0
for ARG in "$@"; do
    if [ "$NEXT_IS_TARGET" -eq 1 ]; then
        TRIPLE="$ARG"
        NEXT_IS_TARGET=0
    else
        case "$ARG" in
            --target) NEXT_IS_TARGET=1 ;;
            --target=*) TRIPLE="${ARG#--target=}" ;;
        esac
    fi
done
if [ -z "$TRIPLE" ] && [ -n "${CARGO_BUILD_TARGET:-}" ]; then
    TRIPLE="$CARGO_BUILD_TARGET"
fi
if [ -n "$TRIPLE" ]; then
    RELEASE_DIR="$RELEASE_BASE/$TRIPLE/release"
else
    RELEASE_DIR="$RELEASE_BASE/release"
fi
APP="$RELEASE_DIR/bundle/macos/Wyrd.app"
if [ ! -d "$APP" ]; then
    echo "error: built bundle not found at $APP." >&2
    exit 1
fi

# Fail closed on signature: ad-hoc verifies clean, and a broken
# bundle must never reach /Applications.
codesign --verify --deep --strict "$APP" || {
    echo "error: built bundle fails codesign verification." >&2
    exit 1
}

# Install: atomic replace via ditto (Apple's bundle copy), then
# verify the installed copy. Quarantine is cleared best-effort —
# a local build carries none, but a copy that does would hesitate
# on first launch.
mkdir -p "$(dirname "$DEST")"
rm -rf "$DEST"
ditto "$APP" "$DEST"
codesign --verify --deep --strict "$DEST" || {
    echo "error: installed bundle at $DEST fails codesign verification." >&2
    exit 1
}
xattr -dr com.apple.quarantine "$DEST" 2>/dev/null || true

echo "installed $DEST (ad-hoc, from $APP)"
echo "note: Finder integration stays inert ad-hoc (no-group-container):"
echo "create, status, settings, and the tray work; Finder needs the"
echo "team-signed path in packaging/macos/README.md."
if [ "$OPEN_APP" -eq 1 ]; then
    open "$DEST"
else
    echo "open \"$DEST\" to launch."
fi
