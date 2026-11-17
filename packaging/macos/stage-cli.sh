#!/bin/sh
# Stage the version-matched `wyrd` CLI for the Tauri bundler.
#
# Runs as part of `build.beforeBundleCommand` (macOS only): builds
# the release CLI and leaves it at the fixed staging path that
# `bundle.macOS.files` copies into `Contents/Resources` before the
# outer sign. The desktop resolves it through `bundled_cli_path`
# (`Contents/MacOS` -> `Contents/Resources/wyrd`); the sibling
# fallback covers devenv/`cargo tauri dev` only. A post-sign copy
# would break the installed verify, so the copy rides the bundler
# mapping, never this script.
#
# Exits 0 on non-macOS platforms so Linux bundles are unaffected.
# No app entitlements, no profile here — the CLI signs with the
# same SIGN_IDENTITY the appex uses (default ad-hoc `-`), paired
# with the outer identity by stage-appex.sh, which runs first and
# refuses a mixed pair fail-closed.
#
# Environment passthrough: SIGN_IDENTITY (default: ad-hoc `-`),
# TOOLCHAIN, SDK, ARCH, CARGO_TARGET_DIR, CARGO_BUILD_TARGET,
# TAURI_ENV_TARGET_TRIPLE (native per-arch builds only, like the
# appex stage), WYRD_NO_MOUNT (when 1, build the CLI without the
# mount stack: the FUSE backend links macFUSE at load, which a
# clean Mac has no copy of — the ad-hoc bundle is vault-only, and
# the release path keeps mount by leaving this unset).
set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

# macOS only: the CLI ships inside the .app bundle, which never
# builds on other targets.
if [ "$(uname -s)" != "Darwin" ]; then
    echo "note: stage-cli.sh skips non-macOS platform $(uname -s)." >&2
    exit 0
fi

# Architecture lockstep: Tauri exports TAURI_ENV_TARGET_TRIPLE for
# the bundle it builds. A cross-target bundle must not silently
# embed a host-arch CLI — per-arch bundles build on their host.
CLI_TRIPLE=""
if [ -n "${TAURI_ENV_TARGET_TRIPLE:-}" ]; then
    case "$TAURI_ENV_TARGET_TRIPLE" in
        aarch64-apple-darwin|x86_64-apple-darwin)
            CLI_TRIPLE="$TAURI_ENV_TARGET_TRIPLE"
            ;;
        *)
            echo "error: stage-cli.sh cannot satisfy target $TAURI_ENV_TARGET_TRIPLE (native per-arch builds only)." >&2
            exit 1
            ;;
    esac
fi

# Link-order pin (Darwin only, mirroring build-bundle.sh): under the
# default devenv shell the Nix cc wrapper puts a /nix/store
# libiconv on the link line, and a team-signed bundle with hardened
# runtime fails library validation at launch. Discover the Xcode SDK
# through the system tools with the Nix-injected values unset
# process-locally, then append the SDK /usr/lib to RUSTFLAGS.
if [ -z "${DEVELOPER_DIR+x}" ] && [ -z "${SDKROOT+x}" ]; then
    :
else
    echo "note: removing inherited DEVELOPER_DIR/SDKROOT for SDK discovery." >&2
fi
unset DEVELOPER_DIR SDKROOT || {
    echo "error: cannot unset DEVELOPER_DIR/SDKROOT (readonly?)." >&2
    exit 1
}
if [ -n "${TOOLCHAIN:-}" ]; then
    DEVDIR=$TOOLCHAIN
else
    DEVDIR=$(/usr/bin/xcode-select -p 2>/dev/null) || {
        echo "error: no Apple developer tools found." >&2
        echo "Install the macOS Command Line Tools (xcode-select --install)" >&2
        exit 1
    }
fi
if [ -n "${SDK:-}" ]; then
    DISCOVERED_SDK=$SDK
else
    DISCOVERED_SDK=$(DEVELOPER_DIR=$DEVDIR /usr/bin/xcrun --sdk macosx --show-sdk-path 2>/dev/null) || {
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

# Build the CLI in release (same profile the wrapper guards),
# then stage the binary. WYRD_NO_MOUNT=1 drops the mount stack
# (`--no-default-features`, the manifest's no-FUSE mode): the
# mount backend hard-links macFUSE at load, which a clean Mac has
# no copy of, so the ad-hoc bundle is vault-only and starts
# everywhere. Target output follows CARGO_TARGET_DIR and an
# explicit --target triple the same way build-bundle.sh resolves it.
cd "$ROOT"
if [ -n "$CLI_TRIPLE" ]; then
    if [ "${WYRD_NO_MOUNT:-0}" = "1" ]; then
        cargo build --release --no-default-features -p wyrd-cli --target "$CLI_TRIPLE"
    else
        cargo build --release -p wyrd-cli --target "$CLI_TRIPLE"
    fi
elif [ "${WYRD_NO_MOUNT:-0}" = "1" ]; then
    cargo build --release --no-default-features -p wyrd-cli
else
    cargo build --release -p wyrd-cli
fi

RELEASE_BASE="${CARGO_TARGET_DIR:-$ROOT/target}"
if [ -n "$CLI_TRIPLE" ]; then
    TRIPLE="$CLI_TRIPLE"
    if [ -n "${CARGO_BUILD_TARGET:-}" ] && [ "$TRIPLE" != "$CARGO_BUILD_TARGET" ]; then
        echo "error: TAURI_ENV_TARGET_TRIPLE ($TRIPLE) disagrees with CARGO_BUILD_TARGET ($CARGO_BUILD_TARGET)." >&2
        exit 1
    fi
    CLI_BIN="$RELEASE_BASE/$TRIPLE/release/wyrd"
elif [ -n "${CARGO_BUILD_TARGET:-}" ]; then
    CLI_BIN="$RELEASE_BASE/$CARGO_BUILD_TARGET/release/wyrd"
else
    CLI_BIN="$RELEASE_BASE/release/wyrd"
fi
if [ ! -f "$CLI_BIN" ]; then
    echo "error: built CLI not found at $CLI_BIN." >&2
    exit 1
fi

STAGE_DIR="$HERE/stage"
mkdir -p "$STAGE_DIR"
STAGED="$STAGE_DIR/wyrd"
cp "$CLI_BIN" "$STAGED"
chmod +x "$STAGED"

# Fail closed on linkage before signing: the shipped CLI must link
# no /nix/store path (same rule the wrapper guards for the bundle).
# Capture once and match without a pipe: under `pipefail` a
# `grep -q` early exit would SIGPIPE otool into a spurious pass.
# With WYRD_NO_MOUNT=1 the CLI must additionally link no FUSE at
# all — the vault-only binary that starts on a clean Mac (this is
# the mechanical form of the B2 repair: a strong libfuse3 entry
# fails the stage with its cause named).
# otool absent means not a macOS bundle host — skip with a note,
# following check-no-nix-links.sh.
if command -v otool >/dev/null 2>&1; then
    LINKED=$(otool -L "$STAGED")
    case "$LINKED" in
        *"/nix/store"*)
            echo "error: staged CLI links /nix/store paths:" >&2
            printf '%s\n' "$LINKED" | grep '/nix/store' >&2 || true
            exit 1
            ;;
    esac
    if [ "${WYRD_NO_MOUNT:-0}" = "1" ]; then
        case "$LINKED" in
            *[Ff][Uu][Ss][Ee]*)
                echo "error: vault-only staged CLI links FUSE ($WYRD_NO_MOUNT=1):" >&2
                printf '%s\n' "$LINKED" | grep -i fuse >&2 || true
                exit 1
                ;;
        esac
    fi
else
    echo "note: otool absent, skipping staged-CLI linkage check." >&2
fi

# Sign with the same identity the appex uses (default ad-hoc `-`),
# but WITHOUT hardened runtime: a mount-enabled CLI loads the
# third-party macFUSE library at runtime, and a hardened-runtime
# signature fails library validation against the differently-signed
# macFUSE dylib (dead in dyld before main — observed 2026-11-15).
# The appex keeps `--options runtime` because `fileproviderd`
# demands it; a helper that must stay loadable does not take it.
# The bundler seals the outer app over the staged tree, and the
# install guard verifies the sealed copy deep-strict.
SIGN_IDENTITY=${SIGN_IDENTITY:--}
codesign --force --sign "$SIGN_IDENTITY" "$STAGED"

echo "staged $STAGED"
codesign -dv "$STAGED" 2>&1 | sed -n '1,4p'

# Stage the supervised-owner wrapper beside the CLI: the daemon
# resolves `wyrd-supervise` as a sibling of the running `wyrd`
# (`sibling_bin` in wyrd-daemon's bootstrap), and the desktop
# passes no explicit `--supervise-bin` in production — so a bundle
# without this sibling fails every managed bootstrap and
# reconfigure at validate. Same source the Nix drive module
# inlines (`nix/drive/supervise.py`, also exposed as the flake's
# `wyrd-supervise` package); the staged copy is that script with a
# `python3` shebang prepended, executable like the Nix wrapper. A
# managed service on the target Mac therefore needs a working
# `python3` — true on every Mac with the Xcode/CLT toolchain the
# installer already requires. Content check, not linkage: a script
# has no Mach-O to otool, but it must never hardcode a /nix/store
# path the way a Nix-built binary would.
STAGED_SUPERVISE="$STAGE_DIR/wyrd-supervise"
{
    printf '#!/usr/bin/env python3\n'
    cat "$ROOT/nix/drive/supervise.py"
} > "$STAGED_SUPERVISE"
chmod +x "$STAGED_SUPERVISE"
if grep -q '/nix/store' "$STAGED_SUPERVISE"; then
    echo "error: staged wyrd-supervise names /nix/store paths." >&2
    exit 1
fi
codesign --force --sign "$SIGN_IDENTITY" "$STAGED_SUPERVISE"

echo "staged $STAGED_SUPERVISE"
codesign -dv "$STAGED_SUPERVISE" 2>&1 | sed -n '1,4p'
