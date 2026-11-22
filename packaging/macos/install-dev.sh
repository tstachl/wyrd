#!/bin/sh
# Developer install for v0.4 (ad-hoc, no fee).
#
# Usage: packaging/macos/install-dev.sh [options] [-- <build args>]
#   --dest <path>                install location (default: /Applications/Wyrd.app
#                                when writable, else ~/Applications/Wyrd.app;
#                                DEST env overrides)
#   --no-open                    build and install without launching the app
#   --sign-identity <identity>   Developer identity for the app, appex, CLI
#                                and domain registrar. With this option a
#                                provisioning profile is required: an ad-hoc
#                                appex cannot load in fileproviderd. The same
#                                value signs the outer app (SIGN_IDENTITY and
#                                APPLE_SIGNING_IDENTITY are both set).
#   --provisioning-profile <p>   .provisionprofile for the appex, carrying
#                                group.md.enesis.wyrd. Kept outside the repo;
#                                the absolute path is passed to the stage hook.
#   --app-provisioning-profile <p>  .provisionprofile for the app (bundle id
#                                md.enesis.wyrd), also carrying the App Group.
#                                The entitled app materializes the container
#                                at launch.
#   -- <args>                    forwarded verbatim to build-bundle.sh
#                                (e.g. -- --target aarch64-apple-darwin). The
#                                wrapper's app-only default is kept unless the
#                                caller forwards --bundles explicitly.
#
# SIGN_IDENTITY / APPLE_SIGNING_IDENTITY / PROVISIONING_PROFILE /
# APP_PROVISIONING_PROFILE in the environment are equivalent to the
# options above.
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
# Finder boundary (honest, not a bug): without --sign-identity and
# --provisioning-profile the appex is ad-hoc, logs no-group-container
# and stays inert — the App Group container only materializes under a
# team-signed build with provisioning. The UI, owner, registry, and
# managed service reinstall work ad-hoc. Pass both options to get a
# Finder-capable build; the domain registrar shipped in the bundle
# registers one File Provider domain per drive that publishes its
# group socket. See packaging/macos/README.md ("Developer install"
# and "Production entitlements and provisioning").
set -eu
set -o pipefail 2>/dev/null || true

usage() {
    echo "usage: packaging/macos/install-dev.sh [options] [-- <build args>]"
    echo ""
    echo "options: --dest <path>, --no-open, --sign-identity <identity>,"
    echo "         --provisioning-profile <path>, --app-provisioning-profile <path>,"
    echo "         --help"
    echo ""
    echo "Builds Wyrd.app from this worktree and installs it to /Applications"
    echo "(or ~/Applications) ready to launch. The bundler hook stages the"
    echo "appex, the domain registrar, and the version-matched wyrd CLI;"
    echo "the desktop resolves the bundled CLI first. The CLI is vault-only"
    echo "(WYRD_NO_MOUNT=1): mount needs macFUSE and a mount-enabled build."
    echo ""
    echo "Ad-hoc builds leave Finder inert (no App Group container). Pass"
    echo "--sign-identity and --provisioning-profile for a Finder-capable"
    echo "build; see packaging/macos/README.md."
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
# Signing inputs: either the options below or the inherited environment.
# Both must be set together for a Finder-capable build, and an outer
# identity can be inferred from the shared inner identity.
SIGN_IDENTITY="${SIGN_IDENTITY:-}"
APPLE_SIGNING_IDENTITY="${APPLE_SIGNING_IDENTITY:-}"
PROVISIONING_PROFILE="${PROVISIONING_PROFILE:-}"
APP_PROVISIONING_PROFILE="${APP_PROVISIONING_PROFILE:-}"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --sign-identity)
            [ -n "${2:-}" ] || { echo "error: --sign-identity needs a value." >&2; exit 1; }
            SIGN_IDENTITY="$2"
            shift 2
            ;;
        --sign-identity=*)
            SIGN_IDENTITY="${1#--sign-identity=}"
            shift
            ;;
        --provisioning-profile)
            [ -n "${2:-}" ] || { echo "error: --provisioning-profile needs a path." >&2; exit 1; }
            PROVISIONING_PROFILE="$2"
            shift 2
            ;;
        --provisioning-profile=*)
            PROVISIONING_PROFILE="${1#--provisioning-profile=}"
            shift
            ;;
        --app-provisioning-profile)
            [ -n "${2:-}" ] || { echo "error: --app-provisioning-profile needs a path." >&2; exit 1; }
            APP_PROVISIONING_PROFILE="$2"
            shift 2
            ;;
        --app-provisioning-profile=*)
            APP_PROVISIONING_PROFILE="${1#--app-provisioning-profile=}"
            shift
            ;;
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
            echo "error: unknown install flag: $1 (usage: --dest, --no-open, --sign-identity, --provisioning-profile, --app-provisioning-profile, --, --help)." >&2
            exit 1
            ;;
        *)
            break
            ;;
    esac
done

# Signing pair: a team identity without a profile produces an appex
# whose App Group entitlement is not effective, so the container
# never materializes and Finder integration cannot work. Fail before
# any build rather than shipping a silently inert app. Ad-hoc needs
# neither, and stays the default for machines without a team.
case "$SIGN_IDENTITY" in
    ""|-)
        if [ -n "$PROVISIONING_PROFILE" ] || [ -n "$APP_PROVISIONING_PROFILE" ]; then
            echo "error: a provisioning profile needs --sign-identity (ad-hoc cannot use a profile)." >&2
            exit 1
        fi
        FINDER_CAPABLE=0
        # Unset, never empty: an empty APPLE_SIGNING_IDENTITY is an
        # explicit identity to Tauri and breaks its ad-hoc default.
        unset APPLE_SIGNING_IDENTITY || true
        ;;
    *)
        if [ -z "$PROVISIONING_PROFILE" ]; then
            echo "error: --sign-identity needs --provisioning-profile carrying group.md.enesis.wyrd." >&2
            exit 1
        fi
        [ -f "$PROVISIONING_PROFILE" ] || {
            echo "error: provisioning profile not found: $PROVISIONING_PROFILE" >&2
            exit 1
        }
        PROFILE_DIR=$(CDPATH= cd -- "$(dirname -- "$PROVISIONING_PROFILE")" && pwd)
        PROVISIONING_PROFILE="$PROFILE_DIR/$(basename -- "$PROVISIONING_PROFILE")"
        if [ -z "$APP_PROVISIONING_PROFILE" ]; then
            echo "error: --sign-identity also needs --app-provisioning-profile: the app must be provisioned for group.md.enesis.wyrd." >&2
            exit 1
        fi
        [ -f "$APP_PROVISIONING_PROFILE" ] || {
            echo "error: app provisioning profile not found: $APP_PROVISIONING_PROFILE" >&2
            exit 1
        }
        APP_PROFILE_DIR=$(CDPATH= cd -- "$(dirname -- "$APP_PROVISIONING_PROFILE")" && pwd)
        APP_PROVISIONING_PROFILE="$APP_PROFILE_DIR/$(basename -- "$APP_PROVISIONING_PROFILE")"
        [ -z "$APPLE_SIGNING_IDENTITY" ] && APPLE_SIGNING_IDENTITY="$SIGN_IDENTITY"
        if [ "$APPLE_SIGNING_IDENTITY" != "$SIGN_IDENTITY" ]; then
            echo "error: APPLE_SIGNING_IDENTITY ($APPLE_SIGNING_IDENTITY) differs from SIGN_IDENTITY ($SIGN_IDENTITY); the bundle must sign as one identity." >&2
            exit 1
        fi
        export APPLE_SIGNING_IDENTITY
        FINDER_CAPABLE=1
        ;;
esac
export SIGN_IDENTITY PROVISIONING_PROFILE APP_PROVISIONING_PROFILE

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

# The extension and the domain registrar are part of the bundle
# contract, not optional staging. A dropped bundler mapping used to
# install silently: check them here so the app never ships without
# the pieces Finder depends on.
APPEX="$APP/Contents/PlugIns/WyrdFileProvider.appex"
REGISTRAR="$APP/Contents/MacOS/WyrdDomainRegistrar"
for BUNDLED_FILE in \
    "$APPEX/Contents/Info.plist" \
    "$APPEX/Contents/MacOS/WyrdFileProvider" \
    "$REGISTRAR"
do
    if [ ! -f "$BUNDLED_FILE" ]; then
        echo "error: built bundle is missing $BUNDLED_FILE." >&2
        echo "The bundler mapping in tauri.conf.json must stage the appex and domain registrar before the outer sign." >&2
        exit 1
    fi
done
if [ ! -x "$REGISTRAR" ]; then
    echo "error: bundled domain registrar is not executable: $REGISTRAR" >&2
    exit 1
fi
if [ "$FINDER_CAPABLE" -eq 1 ]; then
    if [ ! -f "$APPEX/Contents/embedded.provisionprofile" ]; then
        echo "error: team-signed appex has no embedded.provisionprofile." >&2
        exit 1
    fi
    APPEX_INFO=$(codesign -dv --entitlements - "$APPEX" 2>&1) || {
        echo "error: cannot read the embedded appex signature." >&2
        exit 1
    }
    case "$APPEX_INFO" in
        *"Identifier=md.enesis.wyrd.fileprovider"*) ;;
        *) echo "error: embedded appex has the wrong signing identifier." >&2; exit 1 ;;
    esac
    case "$APPEX_INFO" in
        *"TeamIdentifier="*"not set"*)
            echo "error: team-signed appex reports no TeamIdentifier." >&2
            exit 1
            ;;
    esac
    case "$APPEX_INFO" in
        *"group.md.enesis.wyrd"*) ;;
        *) echo "error: embedded appex does not carry the App Group entitlement." >&2; exit 1 ;;
    esac
    REGISTRAR_INFO=$(codesign -dv --entitlements - "$REGISTRAR" 2>&1) || {
        echo "error: cannot read the domain registrar signature." >&2
        exit 1
    }
    case "$REGISTRAR_INFO" in
        *"Identifier=md.enesis.wyrd"*) ;;
        *) echo "error: domain registrar must carry the app's signing identifier." >&2; exit 1 ;;
    esac
    case "$REGISTRAR_INFO" in
        *"group.md.enesis.wyrd"*) ;;
        *) echo "error: domain registrar does not carry the App Group entitlement." >&2; exit 1 ;;
    esac
    if [ ! -f "$APP/Contents/embedded.provisionprofile" ]; then
        echo "error: team-signed app has no embedded.provisionprofile." >&2
        exit 1
    fi
    APP_INFO=$(codesign -dv --entitlements - "$APP" 2>&1) || {
        echo "error: cannot read the app signature." >&2
        exit 1
    }
    case "$APP_INFO" in
        *"group.md.enesis.wyrd"*) ;;
        *) echo "error: the app does not carry the App Group entitlement." >&2; exit 1 ;;
    esac
fi

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

# Domain reconciliation runs from a per-user launch agent, not from a
# resident Wyrd.app process. Fill the helper path from the installed
# bundle, unload any previous copy of this agent (a reinstall must not
# leave the old bundle's path live), then load the new one. The helper
# itself is idempotent, so one pass every 5 seconds is enough.
REGISTRAR_AGENT_TEMPLATE="$HERE/launchd/md.enesis.wyrd.domain-registrar.plist"
REGISTRAR_AGENT="$HOME/Library/LaunchAgents/md.enesis.wyrd.domain-registrar.plist"
if [ ! -f "$REGISTRAR_AGENT_TEMPLATE" ]; then
    echo "error: missing registrar launch agent template: $REGISTRAR_AGENT_TEMPLATE." >&2
    exit 1
fi
mkdir -p "$HOME/Library/LaunchAgents"
REGISTRAR_AGENT_NEW="$REGISTRAR_AGENT.new.$$"
sed "s|@WYRD_REGISTRAR_PATH@|$DEST/Contents/MacOS/WyrdDomainRegistrar|g" \
    "$REGISTRAR_AGENT_TEMPLATE" > "$REGISTRAR_AGENT.new.$$"
if launchctl list md.enesis.wyrd.domain-registrar >/dev/null 2>&1; then
    launchctl unload -w "$REGISTRAR_AGENT" 2>/dev/null || true
fi
if ! mv -- "$REGISTRAR_AGENT_NEW" "$REGISTRAR_AGENT"; then
    echo "error: cannot install the registrar launch agent at $REGISTRAR_AGENT." >&2
    rm -f -- "$REGISTRAR_AGENT_NEW"
    exit 1
fi
if ! launchctl load -w "$REGISTRAR_AGENT"; then
    echo "error: the registrar launch agent was installed but did not load." >&2
    echo "run: launchctl load -w $REGISTRAR_AGENT" >&2
    exit 1
fi

if [ "$FINDER_CAPABLE" -eq 1 ]; then
    echo "installed $DEST (team-signed, Finder-capable, from $APP)"
    echo "note: a drive appears in Finder once its owner publishes the group socket and the registrar agent runs."
else
    echo "installed $DEST (ad-hoc, from $APP)"
    echo "note: Finder integration stays inert ad-hoc (no-group-container):"
    echo "create, status, settings, and managed service reinstall work; pass"
    echo "--sign-identity and --provisioning-profile for a Finder-capable build."
fi
if [ "$OPEN_APP" -eq 1 ]; then
    open "$DEST"
else
    echo "open \"$DEST\" to launch."
fi
