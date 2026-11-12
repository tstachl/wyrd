#!/bin/sh
# Stage 2 Swift harness: compile the adapter sources with a test
# driver and run it against a stub owner. macOS only (FileProvider
# links AppKit-adjacent frameworks unavailable on Linux); CI keeps
# the contract test as the automated leg, this script is
# human-verified evidence. Works in the default devenv shell and
# under `devenv --profile native-apple shell` alike.
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SOURCES="$HERE/../Sources"
OUT=${1:-"$HERE/build/harness"}
SOCK="$OUT/stub.sock"

# Resolve vectors.json from the worktree regardless of the
# invocation directory: Harness/../.. is packaging/macos, so the
# workspace root is one level up.
WORKSPACE=$(CDPATH= cd -- "$HERE/../../../.." && pwd)
VECTORS="$WORKSPACE/crates/wyrd-owner-client/vectors.json"

ARCH=${ARCH:-$(uname -m)}
TOOLCHAIN=${TOOLCHAIN:-}
SWIFTC=${SWIFTC:-}
SDK=${SDK:-}

XCODE_SELECT=/usr/bin/xcode-select
XCRUN=/usr/bin/xcrun

if [ -n "${DEVELOPER_DIR+x}" ] || [ -n "${SDKROOT+x}" ]; then
    echo "note: removing inherited DEVELOPER_DIR/SDKROOT for this build." >&2
fi
unset DEVELOPER_DIR SDKROOT || {
    echo "error: cannot establish a clean Apple toolchain environment." >&2
    exit 1
}
if [ -n "${TOOLCHAIN:-}" ]; then
    DEVDIR=$TOOLCHAIN
else
    DEVDIR=$($XCODE_SELECT -p 2>/dev/null) || {
        echo "error: no Apple developer tools found." >&2
        exit 1
    }
fi
SWIFTC=${SWIFTC:-$(DEVELOPER_DIR=$DEVDIR $XCRUN --sdk macosx --find swiftc 2>/dev/null)}
SDK=${SDK:-$(DEVELOPER_DIR=$DEVDIR $XCRUN --sdk macosx --show-sdk-path 2>/dev/null)}

mkdir -p "$OUT"
# The Harness compiles the adapter sources unchanged (the same set
# the appex build compiles) plus the test driver: production code
# never special-cases the harness beyond the documented socket
# override seam.
# shellcheck disable=SC2046
"$SWIFTC" \
    -target "$ARCH-apple-macos13.0" \
    -sdk "$SDK" \
    -O \
    -o "$OUT/harness" \
    "$SOURCES"/*.swift "$HERE/main.swift"

rm -f "$SOCK"
python3 "$HERE/stub_owner.py" "$SOCK" "$VECTORS" >"$OUT/stub.log" 2>&1 &
STUB_PID=$!
trap 'kill $STUB_PID 2>/dev/null || true' EXIT INT TERM
# Wait for the stub's listen line (bounded): connecting before the
# bind races the server, never the assertions.
for _ in $(seq 1 50); do
    if grep -q "stub listening" "$OUT/stub.log" 2>/dev/null; then
        break
    fi
    sleep 0.1
done
grep -q "stub listening" "$OUT/stub.log" || {
    echo "error: the stub never listened:" >&2
    cat "$OUT/stub.log" >&2
    exit 1
}
"$OUT/harness" "$VECTORS" "$SOCK"
