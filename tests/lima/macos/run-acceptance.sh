#!/usr/bin/env bash
# Host half of the macOS File Provider acceptance scenario.
# Assumes the instance was created from macos-27.yaml and that the one-time
# profile/TCC grants described in README.md are already in place.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SCENARIO="$ROOT/tests/lima/macos"
APP=""
INSTANCE="macos27"
REVISION="unknown"
GROUP="${WYRD_APP_GROUP:-}"
# A fresh drive per run: reusing a name would collide with the identity and
# domain left by a previous attempt. The name is also a valid registry token.
DRIVE="accept-$(date -u +%Y%m%dT%H%M%SZ | tr A-Z a-z)"
# `app` creates the drive through the same CLI sequence the desktop
# onboarding and add-drive views use (identity commit + bootstrap with a
# registry entry); `cli` keeps the original identity-generate/init leg.
CREATE_MODE="${WYRD_CREATE_MODE:-app}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) APP="$2"; shift 2 ;;
    --instance) INSTANCE="$2"; shift 2 ;;
    --revision) REVISION="$2"; shift 2 ;;
    --group) GROUP="$2"; shift 2 ;;
    --drive) DRIVE="$2"; shift 2 ;;
    --create-mode) CREATE_MODE="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
[[ -d "$APP" ]] || { echo "--app must name a built Wyrd.app" >&2; exit 2; }
[[ -n "$GROUP" ]] || { echo "--group or WYRD_APP_GROUP is required" >&2; exit 2; }
[[ "$CREATE_MODE" == "app" || "$CREATE_MODE" == "cli" ]] \
  || { echo "--create-mode must be app or cli" >&2; exit 2; }
command -v limactl >/dev/null || { echo "limactl not found" >&2; exit 2; }

codesign --verify --deep --strict "$APP"
limactl list | grep -qE "^${INSTANCE}[[:space:]]+Running" || limactl start "$INSTANCE"
GUEST_HOME="$(limactl shell "$INSTANCE" -- /bin/sh -c 'printf "%s" "$HOME"')"

# Stage the RC bundle. Removing the old bundle first avoids a mixed-version
# plugin; the domain itself is never removed.
limactl shell "$INSTANCE" -- /bin/sh -c 'rm -rf /Applications/Wyrd.app'
limactl cp --backend=rsync -r "$APP" "$INSTANCE:/Applications/"

# Add-only registration, then the launch agent that reconciles domains.
limactl shell "$INSTANCE" -- /usr/bin/pluginkit -a \
  /Applications/Wyrd.app/Contents/PlugIns/WyrdFileProvider.appex
sed -e 's|@WYRD_REGISTRAR_PATH@|/Applications/Wyrd.app/Contents/MacOS/WyrdDomainRegistrar|g' \
    -e "s|@WYRD_APP_GROUP@|$GROUP|g" \
    "$ROOT/packaging/macos/launchd/md.enesis.wyrd.domain-registrar.plist" \
  | limactl shell "$INSTANCE" -- /bin/sh -c \
      'mkdir -p "$HOME/Library/LaunchAgents"; \
       cat > "$HOME/Library/LaunchAgents/md.enesis.wyrd.domain-registrar.plist"'
limactl shell "$INSTANCE" -- /bin/sh -c \
  'launchctl unload -w "$HOME/Library/LaunchAgents/md.enesis.wyrd.domain-registrar.plist" 2>/dev/null || true; \
   launchctl load -w "$HOME/Library/LaunchAgents/md.enesis.wyrd.domain-registrar.plist"'

# Fresh drive and identity. `app` runs the adapter path the desktop views
# use: identity commit writes the credential pair, then drive bootstrap
# initializes the store and writes the registry entry so the desktop gate
# finds a registered drive at launch. The clean guest has no Command Line
# Tools, so the Python supervise wrapper cannot start a managed service;
# the desktop's --no-service posture registers without one. Bootstrap's
# socket verification is expected to fail before the owner is started
# below, so the leg asserts the registry entry instead of its exit code.
# `cli` keeps the original generate/init leg, which has no registry entry
# and therefore exercises alias-based discovery and the adoption gate.
if [[ "$CREATE_MODE" == "app" ]]; then
  limactl shell "$INSTANCE" -- /bin/sh -s <<EOF
set -e
D="\$HOME/wyrd-accept-$DRIVE"
W=/Applications/Wyrd.app/Contents/Resources/wyrd
mkdir -p "\$D/$DRIVE"
cd "\$D"
printf '%s\n' 'wyrd-acceptance-passphrase' | "\$W" identity commit --identity-file "\$D/identity.bin" --passphrase-file "\$D/passphrase.txt"
set +e
"\$W" drive "\$D/$DRIVE" \
    --identity-file "\$D/identity.bin" --passphrase-file "\$D/passphrase.txt" \
    bootstrap --name "$DRIVE" --mode vault --relay wss://relay.primal.net \
    --no-service > "\$D/bootstrap.log" 2>&1
echo "bootstrap_rc=\$? (socket verification precedes the manual owner start)"
set -e
grep -q '"$DRIVE"' "\$HOME/Library/Application Support/Wyrd/drives.json"
WYRD_APP_GROUP="$GROUP" nohup "\$W" owner "\$D/$DRIVE" \
    --relay wss://relay.primal.net \
    --identity-file "\$D/identity.bin" --passphrase-file "\$D/passphrase.txt" \
    start > "\$D/owner.log" 2>&1 &
sleep 12
EOF
else
  limactl shell "$INSTANCE" -- /bin/sh -s <<EOF
set -e
D="\$HOME/wyrd-accept-$DRIVE"
W=/Applications/Wyrd.app/Contents/Resources/wyrd
mkdir -p "\$D/$DRIVE"
cd "\$D"
"\$W" identity generate --identity-file "\$D/identity.bin" --passphrase-file "\$D/passphrase.txt"
"\$W" drive "\$D/$DRIVE" --identity-file "\$D/identity.bin" --passphrase-file "\$D/passphrase.txt" init
EOF
fi

# Launch after creation so the first-run gate reads the drive the scenario
# just set up. The driver restarts the app later for the restart subset.
limactl shell "$INSTANCE" -- /usr/bin/open -a /Applications/Wyrd.app
sleep 8
if [[ "$CREATE_MODE" == "app" ]]; then
  # Evidence that the shell opened on the drive rather than the welcome
  # guide. The gate decision itself is pinned by desktop unit tests.
  limactl shell "$INSTANCE" -- /bin/sh -c \
    "/usr/sbin/screencapture -x \"\$HOME/wyrd-accept-$DRIVE-evidence/desktop-after-launch.png\"" || true
fi

EVID="${TMPDIR:-/private/var/folders/lw/hzz10xqd7wn18gzsnh4yp5pm0000gn/T/opencode/}wyrd-macos-acceptance-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$EVID"
APPEX="$APP/Contents/PlugIns/WyrdFileProvider.appex/Contents/MacOS/WyrdFileProvider"
APPEX_HASH="$(shasum -a 256 "$APPEX" | awk '{print $1}')"

# A failing driver exit must not discard the evidence collected so far.
set +e
{
  echo "export D=\"\$HOME/wyrd-accept-$DRIVE\""
  echo "export DRIVE=\"\$D/$DRIVE\""
  echo "export M=\"\$HOME/Library/CloudStorage/Wyrd-$DRIVE\""
  echo "export E=\"\$HOME/wyrd-accept-$DRIVE-evidence\""
  echo "export GROUP=\"$GROUP\""
  echo "export REVISION=\"$REVISION\""
  cat "$SCENARIO/filesystem-acceptance.sh"
} | limactl shell "$INSTANCE" -- /bin/sh -s | tee "$EVID/driver.log"
STATUS=${PIPESTATUS[1]}
set -e

limactl cp --backend=rsync -r "$INSTANCE:$GUEST_HOME/wyrd-accept-$DRIVE-evidence" "$EVID/" || true
limactl shell "$INSTANCE" -- /bin/sh -c \
  'cat "$HOME/Library/Application Support/FileProvider/md.enesis.wyrd.fileprovider/Domains.plist"' \
  > "$EVID/Domains.plist" || true
limactl shell "$INSTANCE" -- /bin/sh -c \
  'cat "$HOME/Library/Application Support/Wyrd/drives.json"' \
  > "$EVID/drives.json" || true
STEPS_LOG=$(find "$EVID" -name steps.log -type f 2>/dev/null | head -1)
CONNECTED=$(grep -m1 'connected_precondition Connected=' "$STEPS_LOG" 2>/dev/null \
  | sed -E 's/.*Connected=([^ ]+).*/\1/' || true)
[[ -n "$CONNECTED" ]] || CONNECTED=unknown
if [[ "$STATUS" -eq 0 && ( "$CONNECTED" == "true" || "$CONNECTED" == "1" ) ]]; then
  ACCEPTANCE_PASS=true
else
  ACCEPTANCE_PASS=false
fi
printf '{"revision":"%s","appex_sha256":"%s","app":"%s","instance":"%s","create_mode":"%s","connected_precondition":"%s","acceptance_pass":"%s","status":%s}\n' \
  "$REVISION" "$APPEX_HASH" "$APP" "$INSTANCE" "$CREATE_MODE" "$CONNECTED" "$ACCEPTANCE_PASS" "$STATUS" > "$EVID/metadata.json"
if [[ "$ACCEPTANCE_PASS" == "true" ]]; then
  echo "acceptance claim: all encoded checks passed"
else
  echo "acceptance claim: NOT MET (status=$STATUS, Connected=$CONNECTED; see issue definition of done)"
fi
echo "evidence: $EVID"
exit "$STATUS"
