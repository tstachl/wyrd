#!/bin/sh
# Developer install for v0.4 (ad-hoc, no fee).
#
# Usage: packaging/macos/install-dev.sh [options] [-- <build args>]
#   --dest <path>                install location (default: /Applications/Wyrd.app
#                                when writable, else ~/Applications/Wyrd.app;
#                                DEST env overrides)
#   --no-open                    build and install without launching the app
#   --no-stop                    do not stop already-running Wyrd processes
#                                around the install (default: stop them, so
#                                no process keeps an old binary mapped)
#   --stable-identity <identity>  Apple Development identity for the app,
#                                bundled CLI and domain registrar only. The
#                                appex stays ad-hoc, Finder stays inert, and
#                                no provisioning profile is needed. Keeps the
#                                keychain client stable across rebuilds so
#                                "Always Allow" survives a reinstall.
#   --sign-appex                  With --stable-identity, sign the File
#                                Provider appex with the same identity too.
#                                Still no profile: the team-prefixed App Group
#                                is the unprovisioned macOS form, so the
#                                system checks the team prefix, not a profile.
#                                This gives the appex a container for local
#                                diagnostics records.
#   --sign-identity <identity>   Developer identity for the app, appex, CLI
#                                and domain registrar. With this option a
#                                provisioning profile is required: an ad-hoc
#                                appex cannot load in fileproviderd. The same
#                                value signs the outer app (SIGN_IDENTITY and
#                                APPLE_SIGNING_IDENTITY are both set).
#   --provisioning-profile <p>   .provisionprofile for the appex, carrying
#                                the configured App Group. Kept outside the
#                                repo; the absolute path is passed to the hook.
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
# APP_PROVISIONING_PROFILE / STABLE_SIGN_IDENTITY / WYRD_SIGN_APPEX in
# the environment are equivalent to the options above. WYRD_APP_GROUP (default: the
# team-prefixed id in
# packaging/macos/app-group.sh) selects the App Group rendered into
# the entitlements, Info.plist, registrar agent and Rust runtime.
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
    echo "options: --dest <path>, --no-open, --no-stop, --stable-identity <identity>,"
    echo "         --sign-appex, --sign-identity <identity>, --provisioning-profile <path>,"
    echo "         --app-provisioning-profile <path>, --help"
    echo ""
    echo "Builds Wyrd.app from this worktree and installs it to /Applications"
    echo "(or ~/Applications) ready to launch. The bundler hook stages the"
    echo "appex, the domain registrar, and the version-matched wyrd CLI;"
    echo "the desktop resolves the bundled CLI first. The CLI is vault-only"
    echo "(WYRD_NO_MOUNT=1): mount needs macFUSE and a mount-enabled build."
    echo ""
    echo "Ad-hoc builds leave Finder inert (no App Group container). Pass"
    echo "--stable-identity to keep the keychain client stable across rebuilds"
    echo "(still inert in Finder, no profile). Add --sign-appex to give the"
    echo "extension the same identity and a local diagnostics container. Pass"
    echo "--sign-identity and --provisioning-profile for a Finder-capable build; see"
    echo "packaging/macos/README.md. Running Wyrd processes are stopped"
    echo "around the install unless --no-stop is given."
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
. "$HERE/app-group.sh"
. "$HERE/sign-identity.sh"
GROUP=$(wyrd_app_group)

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
STOP_RUNNING=1
# Signing inputs: explicit flags first, then the script's own environment,
# then the SecretSpec values exported by the devenv shell (keyring). The
# SecretSpec names are stable across shells; these script names remain the
# interface every downstream build hook reads.
# --stable-identity is the narrow keychain-stability mode; --sign-identity
# plus profiles is the all-or-nothing Finder-capable mode. They cannot be
# combined. An outer identity can be inferred from the shared identity.
SIGN_IDENTITY="${SIGN_IDENTITY:-}"
APPLE_SIGNING_IDENTITY="${APPLE_SIGNING_IDENTITY:-}"
STABLE_SIGN_IDENTITY="${STABLE_SIGN_IDENTITY:-}"
PROVISIONING_PROFILE="${PROVISIONING_PROFILE:-${APPLE_PROVISIONING_PROFILE:-}}"
APP_PROVISIONING_PROFILE="${APP_PROVISIONING_PROFILE:-${APPLE_APP_PROVISIONING_PROFILE:-}}"
if [ -n "${APPLE_IDENTITY:-}" ]; then
    if [ -n "$PROVISIONING_PROFILE" ] && [ -n "$APP_PROVISIONING_PROFILE" ]; then
        # Profile-backed: one identity signs the whole bundle.
        SIGN_IDENTITY="${SIGN_IDENTITY:-$APPLE_IDENTITY}"
        APPLE_SIGNING_IDENTITY="${APPLE_SIGNING_IDENTITY:-$APPLE_IDENTITY}"
    else
        # Profile-free: stable identity for app, CLI and registrar; the
        # appex flag decides whether it joins them.
        STABLE_SIGN_IDENTITY="${STABLE_SIGN_IDENTITY:-$APPLE_IDENTITY}"
        WYRD_SIGN_APEX="${WYRD_SIGN_APEX:-${APPLE_SIGN_APEX:-0}}"
    fi
fi
WYRD_SIGN_APEX="${WYRD_SIGN_APEX:-0}"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --stable-identity)
            [ -n "${2:-}" ] || { echo "error: --stable-identity needs a value." >&2; exit 1; }
            STABLE_SIGN_IDENTITY="$2"
            shift 2
            ;;
        --stable-identity=*)
            STABLE_SIGN_IDENTITY="${1#--stable-identity=}"
            [ -n "$STABLE_SIGN_IDENTITY" ] || { echo "error: --stable-identity needs a value." >&2; exit 1; }
            shift
            ;;
        --sign-appex)
            WYRD_SIGN_APPEX=1
            shift
            ;;
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
        --no-stop)
            STOP_RUNNING=0
            shift
            ;;
        --)
            shift
            break
            ;;
        -*)
            echo "error: unknown install flag: $1 (usage: --dest, --no-open, --no-stop, --stable-identity, --sign-appex, --sign-identity, --provisioning-profile, --app-provisioning-profile, --, --help)." >&2
            exit 1
            ;;
        *)
            break
            ;;
    esac
done

# Signing posture: three modes, decided here and exported to every
# staging hook. Default ad-hoc needs no signing assets.
# --stable-identity signs the app, bundled CLI, supervise wrapper and
# domain registrar with one stable Apple Development identity while
# leaving the appex ad-hoc: macOS keeps recognising the keychain client
# across rebuilds, and no profile is involved.
# --sign-appex adds the appex to that stable identity set. Still no
# profile: a team-prefixed App Group is the unprovisioned macOS form, so
# the system checks the team prefix on the signature instead. That gives
# the extension a container for local diagnostics records.
# --sign-identity is the all-or-nothing Finder-capable path and still
# requires both profiles. Refuse any other pairing before any build.
STABLE_MODE=0
APPEX_SIGNED=0
if [ "$WYRD_SIGN_APPEX" != "0" ] && [ "$WYRD_SIGN_APPEX" != "1" ]; then
    echo "error: WYRD_SIGN_APPEX must be 0 or 1." >&2
    exit 1
fi
if [ "$WYRD_SIGN_APPEX" = "1" ] && [ -z "$STABLE_SIGN_IDENTITY" ]; then
    echo "error: --sign-appex needs --stable-identity; it signs the appex with that same identity." >&2
    exit 1
fi
if [ -n "$STABLE_SIGN_IDENTITY" ]; then
    if ! wyrd_is_real_identity "$STABLE_SIGN_IDENTITY"; then
        echo "error: --stable-identity needs a real identity, not $WYRD_ADHOC_IDENTITY." >&2
        exit 1
    fi
    if [ -n "$SIGN_IDENTITY" ] && [ "$SIGN_IDENTITY" != "$STABLE_SIGN_IDENTITY" ]; then
        echo "error: SIGN_IDENTITY ($SIGN_IDENTITY) differs from --stable-identity ($STABLE_SIGN_IDENTITY); use one mode." >&2
        exit 1
    fi
    if [ -n "$APPLE_SIGNING_IDENTITY" ] && [ "$APPLE_SIGNING_IDENTITY" != "$STABLE_SIGN_IDENTITY" ]; then
        echo "error: APPLE_SIGNING_IDENTITY ($APPLE_SIGNING_IDENTITY) differs from --stable-identity ($STABLE_SIGN_IDENTITY); use one mode." >&2
        exit 1
    fi
    if [ -n "$PROVISIONING_PROFILE" ] || [ -n "$APP_PROVISIONING_PROFILE" ]; then
        echo "error: --stable-identity takes no provisioning profile; the team-prefixed group is unprovisioned (use --sign-identity for the profile-backed path)." >&2
        exit 1
    fi
    case "$GROUP" in
        group.*)
            echo "error: --stable-identity needs the unprovisioned <team-id>.<group> form; $GROUP needs a profile." >&2
            exit 1
            ;;
    esac
    SIGN_IDENTITY="$STABLE_SIGN_IDENTITY"
    APPLE_SIGNING_IDENTITY="$STABLE_SIGN_IDENTITY"
    APPEX_SIGNED="$WYRD_SIGN_APPEX"
    if [ "$APPEX_SIGNED" -eq 1 ]; then
        APPEX_SIGN_IDENTITY="$SIGN_IDENTITY"
        WYRD_ALLOW_ADHOC_APPEX=0
    else
        APPEX_SIGN_IDENTITY="$WYRD_ADHOC_IDENTITY"
        WYRD_ALLOW_ADHOC_APPEX=1
    fi
    FINDER_CAPABLE=0
    STABLE_MODE=1
    export APPLE_SIGNING_IDENTITY APPEX_SIGN_IDENTITY WYRD_ALLOW_ADHOC_APPEX
elif [ -z "$SIGN_IDENTITY" ] || [ "$SIGN_IDENTITY" = "$WYRD_ADHOC_IDENTITY" ]; then
    if [ -n "$PROVISIONING_PROFILE" ] || [ -n "$APP_PROVISIONING_PROFILE" ]; then
        echo "error: a provisioning profile needs --sign-identity (ad-hoc cannot use a profile)." >&2
        exit 1
    fi
    FINDER_CAPABLE=0
    APPEX_SIGN_IDENTITY="$WYRD_ADHOC_IDENTITY"
    WYRD_ALLOW_ADHOC_APPEX=0
    # Unset, never empty: an empty APPLE_SIGNING_IDENTITY is an
    # explicit identity to Tauri and breaks its ad-hoc default.
    unset APPLE_SIGNING_IDENTITY || true
    export APPEX_SIGN_IDENTITY WYRD_ALLOW_ADHOC_APPEX
else
    if [ -z "$PROVISIONING_PROFILE" ]; then
        echo "error: --sign-identity needs --provisioning-profile carrying $GROUP." >&2
        exit 1
    fi
    [ -f "$PROVISIONING_PROFILE" ] || {
        echo "error: provisioning profile not found: $PROVISIONING_PROFILE" >&2
        exit 1
    }
    PROFILE_DIR=$(CDPATH= cd -- "$(dirname -- "$PROVISIONING_PROFILE")" && pwd)
    PROVISIONING_PROFILE="$PROFILE_DIR/$(basename -- "$PROVISIONING_PROFILE")"
    if [ -z "$APP_PROVISIONING_PROFILE" ]; then
        echo "error: --sign-identity also needs --app-provisioning-profile: the app must be provisioned for $GROUP." >&2
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
    APPEX_SIGN_IDENTITY="$SIGN_IDENTITY"
    WYRD_ALLOW_ADHOC_APPEX=0
    export APPLE_SIGNING_IDENTITY APPEX_SIGN_IDENTITY WYRD_ALLOW_ADHOC_APPEX
    FINDER_CAPABLE=1
fi
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
# The shell cannot tell an ad-hoc appex from a team-signed one by
# profile presence alone: --sign-appex deliberately embeds none. Pass a
# build fact to the wrapper, which stages it for every build path so a
# direct build-bundle.sh run cannot inherit a stale marker.
if [ "$FINDER_CAPABLE" -eq 1 ]; then
    WYRD_FINDER_CAPABILITY=profile
elif [ "$APPEX_SIGNED" -eq 1 ]; then
    WYRD_FINDER_CAPABILITY=identity
else
    WYRD_FINDER_CAPABILITY=adhoc
fi
export WYRD_FINDER_CAPABILITY
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
        *"$GROUP"*) ;;
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
        *"$GROUP"*) ;;
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
        *"$GROUP"*) ;;
        *) echo "error: the app does not carry the App Group entitlement." >&2; exit 1 ;;
    esac
fi

# Stable-identity modes: the keychain client is the bundled CLI the
# desktop and daemon launch, with the app and registrar alongside it.
# A silent fallback to ad-hoc or to a different real identity would
# bring the repeat prompt back, so confirm these three Mach-O artifacts
# carry the requested identity. The appex either stays ad-hoc or takes
# that same identity under --sign-appex; either way no profile is
# embedded. The supervise wrapper is staged-signed, but Tauri's file
# copy drops its signature; it is a Python launcher, not a keychain
# client, so it is excluded here and covered by the presence checks
# below. The deep-strict verify above still covers every artifact.
if [ "$STABLE_MODE" -eq 1 ]; then
    assert_stable_signed() {
        LABEL=$1
        TARGET=$2
        INFO=$(codesign -dvv "$TARGET" 2>&1) || {
            echo "error: cannot read $LABEL signature: $TARGET" >&2
            exit 1
        }
        case "$INFO" in
            *"Signature=adhoc"*|*"TeamIdentifier=not set"*)
                echo "error: $LABEL is ad-hoc; stable identity did not apply: $TARGET" >&2
                exit 1
                ;;
        esac
        # An unprovisioned group only resolves when the signing team is
        # its prefix. A different-team identity would pass the Authority
        # check yet leave the appex without a container.
        TEAM_PREFIX=${GROUP%%.*}
        SIGNING_TEAM=$(printf '%s\n' "$INFO" | sed -n 's/^TeamIdentifier=//p' | head -1)
        if [ -z "$TEAM_PREFIX" ] || [ "$SIGNING_TEAM" != "$TEAM_PREFIX" ]; then
            echo "error: $LABEL signs as team ${SIGNING_TEAM:-<none>}, but $GROUP is prefixed with ${TEAM_PREFIX:-<none>}." >&2
            exit 1
        fi
        case "$INFO" in
            *"Authority=$STABLE_SIGN_IDENTITY"*) ;;
            *)
                echo "error: $LABEL is signed as a different identity than $STABLE_SIGN_IDENTITY: $TARGET" >&2
                echo "Pass the certificate name, as 'Apple Development: …', not a hash." >&2
                exit 1
                ;;
        esac
    }
    assert_stable_signed "app" "$APP"
    assert_stable_signed "bundled CLI" "$APP/Contents/Resources/wyrd"
    assert_stable_signed "domain registrar" "$REGISTRAR"

    if [ "$APPEX_SIGNED" -eq 1 ]; then
        assert_stable_signed "appex" "$APPEX"
    else
        APPEX_INFO=$(codesign -dv "$APPEX" 2>&1) || {
            echo "error: cannot read appex signature: $APPEX" >&2
            exit 1
        }
        case "$APPEX_INFO" in
            *"Signature=adhoc"*) ;;
            *) echo "error: appex must stay ad-hoc unless --sign-appex is given." >&2; exit 1 ;;
        esac
    fi
    if [ -f "$APPEX/Contents/embedded.provisionprofile" ] \
        || [ -f "$APP/Contents/embedded.provisionprofile" ]; then
        echo "error: stable-identity mode embeds no provisioning profile." >&2
        exit 1
    fi
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

# Clean slate: no process may keep the pre-install binary mapped.
# This runs after the install swap. Supervise goes first (while it
# lives it restarts its owner), then the owner so its drive lock is
# released, then the GUI app, registrar and extension. SIGTERM first;
# only those exact PIDs that ignore it get SIGKILL after a bounded
# wait, so a freshly restarted agent is never caught by the
# escalation. The per-drive jobs are kickstarted explicitly below:
# their KeepAlive restarts only exit 0, not our SIGTERM.
stop_running_wyrd() {
    [ "$STOP_RUNNING" -eq 1 ] || return 0

    if command -v osascript >/dev/null 2>&1; then
        osascript -e 'tell application id "md.enesis.wyrd" to quit' \
            >/dev/null 2>&1 || true
    fi

    # A PID can be recycled between SIGTERM and the escalation. Recheck
    # the command line, not just liveness, before sending SIGKILL. This
    # is best-effort: a freshly restarted Wyrd agent can reuse the same
    # pattern and still be caught; a start-time token would be needed
    # for a stronger guarantee.
    is_wyrd_pid() {
        cmdline=$(ps -p "$1" -o command= 2>/dev/null) || return 1
        case "$cmdline" in
            *"/wyrd-supervise "*|*"/wyrd owner "*|*"Wyrd.app/Contents/"*|*"WyrdDomainRegistrar"*|*"WyrdFileProvider.appex"*)
                return 0
                ;;
            *)
                return 1
                ;;
        esac
    }

    STOP_PIDS=""
    stop_matching() {
        matched=$(pgrep -f "$1" 2>/dev/null | tr '\n' ' ') || matched=""
        for pid in $matched; do
            [ "$pid" = "$$" ] && continue
            case " $STOP_PIDS " in
                *" $pid "*) continue ;;
            esac
            STOP_PIDS="$STOP_PIDS $pid"
            desc=$(ps -p "$pid" -o command= 2>/dev/null | cut -c1-100) || desc=""
            echo "  stopping pid $pid: $desc"
            kill -TERM "$pid" 2>/dev/null || true
        done
    }

    echo "note: stopping running Wyrd processes for a clean slate"
    stop_matching '/wyrd-supervise '
    stop_matching '/wyrd owner '
    stop_matching 'Wyrd\.app/Contents/|WyrdDomainRegistrar|WyrdFileProvider\.appex'

    tick=0
    while [ "$tick" -lt 10 ]; do
        survivors=""
        for pid in $STOP_PIDS; do
            if kill -0 "$pid" 2>/dev/null; then survivors="survivor"; fi
        done
        [ -z "$survivors" ] && break
        sleep 0.2
        tick=$((tick + 1))
    done
    for pid in $STOP_PIDS; do
        if kill -0 "$pid" 2>/dev/null && is_wyrd_pid "$pid"; then
            echo "  force-stopping pid $pid"
            kill -KILL "$pid" 2>/dev/null || true
        fi
    done

    # The per-drive launch agents use KeepAlive with SuccessfulExit, so
    # launchd restarts only exit 0 — not our SIGTERM/SIGKILL. Bring each
    # loaded drive back explicitly, against the newly installed bundle.
    # Jobs that are not loaded (login-start disabled) are left alone.
    DRIVE_LABELS=$(launchctl list 2>/dev/null | awk '{print $3}' \
        | grep '^md\.enesis\.wyrd\.drive-' || true)
    RESTART_FAILURES=""
    for label in $DRIVE_LABELS; do
        echo "  restarting $label"
        if ! launchctl kickstart -k "gui/$(id -u)/$label" 2>/dev/null; then
            RESTART_FAILURES="$RESTART_FAILURES $label"
        fi
    done
    if [ -n "$RESTART_FAILURES" ]; then
        # Fail closed: a refused kickstart leaves exactly the stopped
        # state this fix is meant to prevent. Name the recovery command.
        echo "error: could not restart drive agent(s):$RESTART_FAILURES" >&2
        for label in $RESTART_FAILURES; do
            echo "  recover: launchctl kickstart -k gui/$(id -u)/$label" >&2
        done
        return 1
    fi
}

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
sed -e "s|@WYRD_REGISTRAR_PATH@|$DEST/Contents/MacOS/WyrdDomainRegistrar|g" \
    -e "s|@WYRD_APP_GROUP@|$GROUP|g" \
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

# Old binaries are gone from disk now; stop anything still mapping
# them, then kickstart the loaded drive agents against the new bundle.
stop_running_wyrd

# Finder binds a domain to the plugin's bundle identifier, not to its
# path. A stale Xcode DerivedData build with the same identifier can
# remain registered and, when registered later than this install, serve
# the domains with its own baked-in App Group. The owner then publishes
# sockets in one container while the extension probes another — the
# observed 2026-11-24 "container path mismatch" was a shadowed plugin,
# not a path-derivation defect. Detect and report it; never remove
# another build's registration without the developer's say-so.
if [ "$(uname -s)" = "Darwin" ] && [ -x /usr/bin/pluginkit ]; then
    INSTALLED_APPEX="$DEST/Contents/PlugIns/WyrdFileProvider.appex"
    SHADOWED=$(/usr/bin/pluginkit -mAv -i md.enesis.wyrd.fileprovider 2>/dev/null \
        | wyrd_shadowed_plugins "$INSTALLED_APPEX") \
        || SHADOWED=""
    if [ -n "$SHADOWED" ]; then
        echo "warning: other plugins share the File Provider bundle id and may shadow this install:" >&2
        printf '%s\n' "$SHADOWED" | sed 's/^/  /' >&2
        echo "remove each with: /usr/bin/pluginkit -r <path>" >&2
        echo "Finder serves a domain through the most recently registered plugin with that id." >&2
    fi
fi

if [ "$FINDER_CAPABLE" -eq 1 ]; then
    echo "installed $DEST (team-signed, Finder-capable, from $APP)"
    echo "note: a drive appears in Finder once its owner publishes the group socket and the registrar agent runs."
elif [ "$APPEX_SIGNED" -eq 1 ]; then
    echo "installed $DEST (stable identity for app, CLI, registrar and appex: $STABLE_SIGN_IDENTITY; no profile, from $APP)"
    echo "note: the team-prefixed group is unprovisioned, so the appex can resolve its container and keep local diagnostics records."
    echo "Finder may now work without a profile; free-team assets expire after seven days — renew and reinstall then."
elif [ "$STABLE_MODE" -eq 1 ]; then
    echo "installed $DEST (stable identity for app, CLI and registrar: $STABLE_SIGN_IDENTITY; appex ad-hoc, from $APP)"
    echo "note: the keychain client stays the same across rebuilds; Finder stays inert (no-group-container):"
    echo "create, status, settings, and managed service reinstall work; pass"
    echo "--sign-identity and --provisioning-profile for a Finder-capable build."
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
