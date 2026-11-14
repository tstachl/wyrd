#!/usr/bin/env bash
# tests/harness/test-entitlements.sh — pin the production
# entitlements decision: sandbox-plus-group on the appex, group
# alone with no sandbox on the unsandboxed app, one App Group
# everywhere, and the appex provisioning wiring. Linux-safe: static
# assertions only (the real `build-appex.sh` run is macOS-only
# manual evidence).
#
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-entitlements.sh`. (Wired into the CI
# shell-tests job alongside the other stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

APP="$ROOT/packaging/macos/Wyrd.entitlements"
APPEX="$ROOT/packaging/macos/fileprovider/WyrdFileProvider.entitlements"
GROUP='group.md.enesis.wyrd'

# 1. The decision is recorded and names the app entitlement file.
grep -Fq 'Production entitlements and provisioning (decided)' \
  "$ROOT/packaging/macos/README.md" \
  || die "README: decision section missing"
grep -Fq 'packaging/macos/Wyrd.entitlements' "$ROOT/packaging/macos/README.md" \
  || die "README: app entitlements path missing"
pass "README pins the entitlements decision"

# 2. Both files parse as plists; the appex is sandboxed on the
# group, the app carries the group with deliberately no sandbox —
# the desktop spawns the CLI and writes outside any container.
python3 - "$APP" "$APPEX" <<EOF
import plistlib, sys
group = "$GROUP"
with open(sys.argv[1], 'rb') as f:
    app = plistlib.load(f)
with open(sys.argv[2], 'rb') as f:
    appex = plistlib.load(f)
assert appex.get('com.apple.security.app-sandbox') is True, appex
assert group in appex.get('com.apple.security.application-groups', []), appex
assert 'com.apple.security.app-sandbox' not in app, app
assert group in app.get('com.apple.security.application-groups', []), app
EOF
pass "appex sandboxed on the group, app grouped without sandbox"

# 3. Minimal by default, per target: no other com.apple.* key on
# either file. Adding a capability must update this guard with its
# rationale, never slip in silently.
appex_allowed="$(printf 'com.apple.security.app-sandbox\ncom.apple.security.application-groups')"
app_allowed='com.apple.security.application-groups'
appex_keys="$(grep -o 'com\.apple[^<]*' "$APPEX" || true)"
app_keys="$(grep -o 'com\.apple[^<]*' "$APP" || true)"
[[ -n "$appex_keys" && "$appex_keys" == "$(printf '%s' "$appex_allowed" | sort -u)" ]] \
  || die "appex: non-minimal entitlement keys: $appex_keys"
[[ -n "$app_keys" && "$app_keys" == "$app_allowed" ]] \
  || die "app: non-minimal entitlement keys: $app_keys"
pass "entitlement sets stay minimal"

# 4. Appex provisioning wiring: opt-in embed, fail-fast refusal,
# embedded before signing. The `cp` line pins the order, not the
# first textual match (the header comment names the same path).
SCRIPT="$ROOT/packaging/macos/fileprovider/build-appex.sh"
grep -Fq 'PROVISIONING_PROFILE' "$SCRIPT" \
  || die "build-appex.sh: no PROVISIONING_PROFILE wiring"
cp_line=$(grep -n '^    cp "\$PROVISIONING_PROFILE"' "$SCRIPT" | head -1 | cut -d: -f1)
sign_line=$(grep -n '^codesign ' "$SCRIPT" | head -1 | cut -d: -f1)
[[ -n "$cp_line" && -n "$sign_line" && "$cp_line" -lt "$sign_line" ]] \
  || die "build-appex.sh: profile embed must precede codesign"
refuse_line=$(grep -n 'PROVISIONING_PROFILE names no file' "$SCRIPT" | head -1 | cut -d: -f1)
swiftc_line=$(grep -n '"\$SWIFTC"' "$SCRIPT" | head -1 | cut -d: -f1)
[[ -n "$refuse_line" && -n "$swiftc_line" && "$refuse_line" -lt "$swiftc_line" ]] \
  || die "build-appex.sh: profile refusal must precede swiftc"
pass "appex profile embed precedes signing"

echo "entitlements: $PASS checks passed"
