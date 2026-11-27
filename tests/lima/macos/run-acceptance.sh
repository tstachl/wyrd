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
# domain left by a previous attempt.
DRIVE="accept-$(date -u +%Y%m%dT%H%M%SZ | tr A-Z a-z)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) APP="$2"; shift 2 ;;
    --instance) INSTANCE="$2"; shift 2 ;;
    --revision) REVISION="$2"; shift 2 ;;
    --group) GROUP="$2"; shift 2 ;;
    --drive) DRIVE="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,6p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
[[ -d "$APP" ]] || { echo "--app must name a built Wyrd.app" >&2; exit 2; }
[[ -n "$GROUP" ]] || { echo "--group or WYRD_APP_GROUP is required" >&2; exit 2; }
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
limactl shell "$INSTANCE" -- /usr/bin/open -a /Applications/Wyrd.app

# Fresh drive and identity; the guest driver starts its own owner.
limactl shell "$INSTANCE" -- /bin/sh -s <<EOF
set -e
D="\$HOME/wyrd-accept-$DRIVE"
W=/Applications/Wyrd.app/Contents/Resources/wyrd
mkdir -p "\$D/$DRIVE"
cd "\$D"
"\$W" identity generate --identity-file "\$D/identity.bin" --passphrase-file "\$D/passphrase.txt"
"\$W" drive "\$D/$DRIVE" --identity-file "\$D/identity.bin" --passphrase-file "\$D/passphrase.txt" init
EOF

EVID="${TMPDIR:-/private/var/folders/lw/hzz10xqd7wn18gzsnh4yp5pm0000gn/T/opencode/}wyrd-macos-acceptance-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$EVID"
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

limactl cp --backend=rsync -r "$INSTANCE:$GUEST_HOME/wyrd-accept-$DRIVE-evidence" "$EVID/" || true
limactl shell "$INSTANCE" -- /bin/sh -c \
  'cat "$HOME/Library/Application Support/FileProvider/md.enesis.wyrd.fileprovider/Domains.plist"' \
  > "$EVID/Domains.plist" || true
printf '{"revision":"%s","app":"%s","instance":"%s","status":%s}\n' \
  "$REVISION" "$APP" "$INSTANCE" "$STATUS" > "$EVID/metadata.json"
echo "evidence: $EVID"
exit "$STATUS"
