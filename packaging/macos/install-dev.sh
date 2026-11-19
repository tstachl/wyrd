#!/bin/sh
# Developer install for v0.4 (ad-hoc, no fee).
#
# Usage: packaging/macos/install-dev.sh [--dest <path>] [--no-open] [-- <build args>]
#   --dest <path>   install location (default: /Applications/Wyrd.app when
#                   writable, else ~/Applications/Wyrd.app; DEST env overrides)
#   --no-open       build and install without launching the app
#   -- <args>       forwarded verbatim to packaging/macos/build-bundle.sh
#                   (e.g. -- --target aarch64-apple-darwin). The wrapper's
#                   app-only default is kept unless the caller forwards
#                   --bundles explicitly.
#
# What it does: ensures the frontend dependencies, builds Wyrd.app
# ad-hoc through build-bundle.sh (the
# bundler hook stages the appex plus the version-matched `wyrd` CLI
# and its `wyrd-supervise` sibling, pairs identities, and pins
# versions), verifies the signature, and
# installs the bundle to /Applications (or ~/Applications) ready to
# launch. No signing assets required: Xcode/CLT plus the Rust and web
# toolchains only.
#
# Finder boundary (honest, not a bug): an ad-hoc appex logs
# no-group-container and stays inert — the App Group container only
# materializes under a team-signed build with provisioning. The UI,
# owner, registry, and managed service reinstall work ad-hoc; Finder
# integration stays a separately team-gated path (packaging/macos/README.md, "Developer
# install" plus "Production entitlements and provisioning").
set -eu
set -o pipefail 2>/dev/null || true

usage() {
    echo "usage: packaging/macos/install-dev.sh [--dest <path>] [--no-open] [-- <build args>]"
    echo ""
    echo "Builds Wyrd.app ad-hoc and installs it to /Applications"
    echo "(or ~/Applications) ready to launch. The bundler hook stages"
    echo "the appex plus the version-matched wyrd CLI into"
    echo "Contents/Resources/wyrd; the desktop resolves the bundled copy"
    echo "first and the target-dir sibling only in dev. The ad-hoc CLI"
    echo "is vault-only (WYRD_NO_MOUNT=1): mount needs macFUSE and a"
    echo "mount-enabled build."
    echo ""
    echo "Finder stays inert ad-hoc (no-group-container); the team-signed"
    echo "path is in packaging/macos/README.md."
}

# --help before anything that can fail: a machine without cargo can
# still read the usage. Stops at `--` so forwarded build args are
# never mistaken for install flags.
for ARG in "$@"; do
    if [ "$ARG" = "--" ]; then
        break
    fi
    case "$ARG" in
        --help|-h) usage; exit 0 ;;
    esac
done

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

# macOS only: the .app bundle never ships on other targets.
if [ "$(uname -s)" != "Darwin" ]; then
    echo "error: install-dev.sh builds Wyrd.app on macOS only (this host: $(uname -s))." >&2
    exit 1
fi

# Prerequisites, named with the fix. TOOLCHAIN/SDK/SIGN_IDENTITY pass
# through to build-bundle.sh and the stage hooks; nothing here sets them.
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
# forwards verbatim to build-bundle.sh. No interleaving:
# `install-dev.sh --dest … --no-open -- --target <triple>`.
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
        --)
            shift
            break
            ;;
        -*)
            echo "error: unknown install flag: $1 (usage: --dest, --no-open, --, --help)." >&2
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
    /*.app)
        ;;
    *)
        echo "error: --dest must be an absolute path ending in .app: $DEST" >&2
        exit 1
        ;;
esac
# Canonicalize before the build-tree comparison: a lexical check
# is bypassed by `..` (`/Applications/../<repo>/x.app`). Physical
# (`-P`) so symlinked parents (e.g. /tmp) resolve before comparing.
DEST_PARENT_DIR=$(dirname -- "$DEST")
DEST_BASE_NAME=$(basename -- "$DEST")
mkdir -p -- "$DEST_PARENT_DIR"
DEST_PARENT_DIR=$(cd -P -- "$DEST_PARENT_DIR" && pwd -P)
DEST="$DEST_PARENT_DIR/$DEST_BASE_NAME"

# Locate the built bundle before building: this mirrors the
# wrapper's release-dir rule (CARGO_TARGET_DIR-aware,
# --target/CARGO_BUILD_TARGET triple-aware) so the installer
# inspects what the wrapper guarded. The two copies are pinned
# together by tests/harness/test-dev-install.sh. Resolving early
# also lets the destination refusal below run before the build.
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

# Refuse a destination that would delete the build output or the
# source tree: the install target must not equal the built bundle
# and must not sit inside the repository or the target dir.
# Runs before the build, so a refused destination costs no compile.
case "$DEST" in
    "$APP"|"$APP"/*)
        echo "error: --dest must not be the built bundle itself: $DEST" >&2
        exit 1
        ;;
    "$ROOT"/*|"$RELEASE_BASE"/*)
        echo "error: --dest must not sit inside the repository or target dir: $DEST" >&2
        exit 1
        ;;
esac

# Build through the wrapper (link-order pin plus no-Nix-links guard),
# keeping its app-only default unless the caller forwards --bundles.
# The ad-hoc CLI is vault-only: WYRD_NO_MOUNT=1 drops the mount
# stack, whose macFUSE load dependency a clean Mac has no copy of.
#
# Frontend dependencies first: the Tauri beforeBuildCommand runs
# `bun run build` without installing, so a fresh checkout (or a
# lockfile change) fails with a cryptic vite resolve error. Placed
# after the destination refusal above so a refused invocation costs
# no network. Install when node_modules is missing or older than
# the lockfile or manifest; --frozen-lockfile keeps the build on
# the committed set, and its own error names the fix when the
# manifest drifted.
if [ ! -d "$ROOT/apps/desktop/node_modules" ] \
    || [ "$ROOT/apps/desktop/bun.lock" -nt "$ROOT/apps/desktop/node_modules" ] \
    || [ "$ROOT/apps/desktop/package.json" -nt "$ROOT/apps/desktop/node_modules" ]; then
    (cd "$ROOT/apps/desktop" && bun install --frozen-lockfile)
fi
HAS_BUNDLES=0
for ARG in "$@"; do
    case "$ARG" in
        --bundles|--bundles=*) HAS_BUNDLES=1 ;;
    esac
done
if [ "$HAS_BUNDLES" -eq 0 ]; then
    set -- --bundles app "$@"
fi
WYRD_NO_MOUNT=1
export WYRD_NO_MOUNT
"$HERE/build-bundle.sh" "$@"

if [ ! -d "$APP" ]; then
    echo "error: built bundle not found at $APP." >&2
    exit 1
fi

# Fail closed on signature: ad-hoc verifies clean, and a broken
# bundle never reaches /Applications.
codesign --verify --deep --strict "$APP" || {
    echo "error: built bundle fails codesign verification." >&2
    exit 1
}
# The co-installed CLI the desktop resolves first, plus the
# supervised-owner wrapper the daemon resolves as its sibling:
# the fixed pair the acceptance case (create a drive, show it
# Available) depends on, and without the wrapper every managed
# bootstrap and reconfigure fails at validate. Both ride the
# bundler mapping from packaging/macos/stage, so a missing staged
# file fails here, never in the installed app.
for BUNDLED in wyrd wyrd-supervise; do
    if [ ! -f "$APP/Contents/Resources/$BUNDLED" ]; then
        echo "error: built bundle has no Contents/Resources/$BUNDLED." >&2
        echo "The desktop resolves the bundled CLI first and the daemon its supervise sibling; reinstall the matched set." >&2
        exit 1
    fi
    if [ ! -x "$APP/Contents/Resources/$BUNDLED" ]; then
        echo "error: built bundle Contents/Resources/$BUNDLED is not executable." >&2
        echo "The daemon's sibling lookup requires the executable bit." >&2
        exit 1
    fi
done

# Install: copy to a PID-suffixed staging name, verify the copy,
# then swap through a PID-suffixed `.old` name: two concurrent
# runs never share a staging name, the previous install survives
# a failed swap (only our own `.old` is ever removed or
# restored), and a pre-existing staging name refuses instead of
# deleting work another run may still be verifying.
STAGED_DEST="$DEST.new.$$"
OLD_DEST="$DEST.old.$$"
if [ -e "$STAGED_DEST" ] || [ -e "$OLD_DEST" ]; then
    echo "error: staging path exists (another install in flight?): $STAGED_DEST" >&2
    exit 1
fi
ditto "$APP" "$STAGED_DEST"
codesign --verify --deep --strict "$STAGED_DEST" || {
    echo "error: staged bundle at $STAGED_DEST fails codesign verification." >&2
    rm -rf -- "$STAGED_DEST"
    exit 1
}
if [ -e "$DEST" ] && ! mv -- "$DEST" "$OLD_DEST"; then
    echo "error: cannot stage the previous install aside." >&2
    rm -rf -- "$STAGED_DEST"
    exit 1
fi
if mv -- "$STAGED_DEST" "$DEST"; then
    rm -rf -- "$OLD_DEST"
else
    rc=$?
    if [ -e "$OLD_DEST" ]; then
        mv -- "$OLD_DEST" "$DEST"
    fi
    echo "error: install swap failed." >&2
    exit "$rc"
fi
codesign --verify --deep --strict "$DEST" || {
    echo "error: installed bundle at $DEST fails codesign verification." >&2
    exit 1
}
xattr -dr com.apple.quarantine "$DEST" 2>/dev/null || true

echo "installed $DEST (ad-hoc, from $APP)"
echo "note: Finder integration stays inert ad-hoc (no-group-container):"
echo "create, status, settings, and managed service reinstall work; Finder needs the"
echo "team-signed path in packaging/macos/README.md."
if [ "$OPEN_APP" -eq 1 ]; then
    open "$DEST"
else
    echo "open \"$DEST\" to launch."
fi
