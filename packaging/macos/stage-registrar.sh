#!/bin/sh
# Stage the File Provider domain registrar for the Tauri bundler.
#
# Runs in `build.beforeBundleCommand` beside stage-appex.sh and
# stage-cli.sh (macOS only). The helper lands at the fixed staging path
# that `bundle.macOS.files` copies into Contents/MacOS before the
# outer sign, so no post-sign copy is needed.
#
# Identity pairing is owned by stage-appex.sh, which runs first and
# refuses a mixed inner/outer pair unless the stable-identity dev mode
# opts out; this script uses the shared SIGN_IDENTITY (default ad-hoc
# `-`) for the registrar, so a stable keychain client stays stable.
set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if [ "$(uname -s)" != "Darwin" ]; then
    echo "note: stage-registrar.sh skips non-macOS platform $(uname -s)." >&2
    exit 0
fi

# Native per-arch bundles only, matching the appex and CLI stages.
if [ -n "${TAURI_ENV_TARGET_TRIPLE:-}" ]; then
    case "$TAURI_ENV_TARGET_TRIPLE" in
        aarch64-apple-darwin) ARCH=arm64 ;;
        x86_64-apple-darwin) ARCH=x86_64 ;;
        *)
            echo "error: stage-registrar.sh cannot satisfy target $TAURI_ENV_TARGET_TRIPLE (native per-arch builds only)." >&2
            exit 1
            ;;
    esac
    export ARCH
fi

STAGE="$HERE/stage/WyrdDomainRegistrar"
mkdir -p "$HERE/stage"
rm -f "$STAGE"
"$HERE/domain-registrar/build-registrar.sh" "$HERE/stage"

[ -f "$STAGE" ] || {
    echo "error: registrar build produced no $STAGE." >&2
    exit 1
}

echo "staged $STAGE"
