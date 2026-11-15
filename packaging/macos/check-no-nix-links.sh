#!/bin/sh
# Fail-closed guard: the shipped macOS binaries must link no
# /nix/store path, or hardened runtime fails library validation at
# launch (`different Team IDs`, dead before main).
#
# Usage: packaging/macos/check-no-nix-links.sh [repo-root [release-dir]]
# Inspects every regular file under the bundle app's Contents/MacOS
# (the Tauri binary under its real filename, never a hardcoded one),
# the standalone release binary, and the embedded appex binary when
# present. Any /nix/store linkage fails the build, and so does
# inspecting nothing at all — a guard that examines nothing must not
# pass. otool absent means not a macOS bundle host: skip with a note.
#
# The release dir defaults to <root>/target/release, or
# $CARGO_TARGET_DIR/release when set; pass it explicitly for
# --target triples, whose artifacts live elsewhere.
set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=${1:-$(cd "$HERE/../.." && pwd)}
TARGET_DIR=${CARGO_TARGET_DIR:-"$ROOT/target"}
RELEASE_DIR=${2:-"$TARGET_DIR/release"}

if ! command -v otool >/dev/null 2>&1; then
    echo "note: otool absent, skipping no-Nix-links guard." >&2
    exit 0
fi

APP_MACOS="$RELEASE_DIR/bundle/macos/Wyrd.app/Contents/MacOS"
STANDALONE_BIN="$RELEASE_DIR/wyrd-desktop"
APPEX_BIN="$RELEASE_DIR/bundle/macos/Wyrd.app/Contents/PlugIns"
APPEX_BIN="$APPEX_BIN/WyrdFileProvider.appex/Contents/MacOS/WyrdFileProvider"

INSPECTED=0
FOUND=0
inspect() {
    LINKED=$(otool -L "$1")
    INSPECTED=$((INSPECTED + 1))
    case "$LINKED" in
        *"/nix/store"*)
            echo "error: $1 links /nix/store paths:" >&2
            printf '%s\n' "$LINKED" | grep '/nix/store' >&2 || true
            FOUND=1
            ;;
    esac
}

if [ -d "$APP_MACOS" ]; then
    for BIN in "$APP_MACOS"/*; do
        if [ -f "$BIN" ]; then
            inspect "$BIN"
        fi
    done
fi
for BIN in "$STANDALONE_BIN" "$APPEX_BIN"; do
    if [ -f "$BIN" ]; then
        inspect "$BIN"
    fi
done

if [ "$INSPECTED" -eq 0 ]; then
    echo "error: no-Nix-links guard inspected nothing (no bundle, no standalone binary)." >&2
    exit 1
fi
if [ "$FOUND" -ne 0 ]; then
    echo "error: no-Nix-links guard failed; see paths above." >&2
    exit 1
fi
echo "note: no-Nix-links guard passed ($INSPECTED binaries)." >&2
