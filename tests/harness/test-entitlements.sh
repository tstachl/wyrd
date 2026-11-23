#!/usr/bin/env bash
# tests/harness/test-entitlements.sh — pin the production
# entitlements decision: no entitlement file on the unsandboxed app,
# sandbox-plus-group on the appex, one App Group, and the appex
# provisioning wiring. Linux-safe: static assertions only (the real
# `build-appex.sh` run is macOS-only manual evidence).
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

APPEX="$ROOT/packaging/macos/fileprovider/WyrdFileProvider.entitlements"
GROUP='JU2RFHWQPH.group.md.enesis.wyrd'
TOKEN='@WYRD_APP_GROUP@'

# 1. The reopened decision is recorded: the unsandboxed app keeps its
# direct paths but joins the one App Group so it can materialize the
# container at launch. The previous reasoning is preserved in prose.
grep -F >/dev/null 'Production entitlements and provisioning (reopened and amended)' \
  "$ROOT/packaging/macos/README.md" \
  || die "README: reopened decision section missing"
grep -F >/dev/null 'Previous app decision (superseded)' \
  "$ROOT/packaging/macos/README.md" \
  || die "README: previous reasoning not preserved"
python3 - "$ROOT/packaging/macos/Wyrd.app.entitlements" <<EOF
import plistlib, sys
group = "$GROUP"
with open(sys.argv[1], 'rb') as f:
    app = plistlib.load(f)
assert set(app) == {'com.apple.security.application-groups'}, app
assert app['com.apple.security.application-groups'] == [group], app
EOF
pass "README pins the reopened decision; app holds only the App Group"

# 2. The appex plist parses and stays sandboxed on the group.
python3 - "$APPEX" <<EOF
import plistlib, sys
group = "$GROUP"
with open(sys.argv[1], 'rb') as f:
    appex = plistlib.load(f)
assert appex.get('com.apple.security.app-sandbox') is True, appex
assert appex.get('com.apple.security.application-groups', []) == ['$TOKEN'], appex
EOF
pass "appex sandboxed on the render token"
grep -F >/dev/null 'wyrd_render_app_group "$HERE/WyrdFileProvider.entitlements"' \
  "$ROOT/packaging/macos/fileprovider/build-appex.sh" \
  || die "build-appex.sh: appex entitlements are not rendered"
grep -F >/dev/null "WYRD_APP_GROUP_DEFAULT='$GROUP'" \
  "$ROOT/packaging/macos/app-group.sh" \
  || die "app-group.sh: default drifted"
pass "build renders WYRD_APP_GROUP into the appex entitlements"

# 3. Minimal by default: no other com.apple.* key on the appex.
# Adding a capability must update this guard with its rationale,
# never slip in silently.
allowed="$(printf 'com.apple.security.application-groups\ncom.apple.security.app-sandbox' | sort -u)"
keys="$(grep -o 'com\.apple[^<]*' "$APPEX" | sort -u || true)"
[[ -n "$keys" && "$keys" == "$allowed" ]] \
  || die "appex: non-minimal entitlement keys: $keys"
pass "appex entitlement set stays minimal"

# 4. Appex provisioning wiring: opt-in embed, fail-fast refusal,
# embedded before signing. The `cp` command pins the order, not a
# header-comment match.
SCRIPT="$ROOT/packaging/macos/fileprovider/build-appex.sh"
grep -F >/dev/null 'PROVISIONING_PROFILE' "$SCRIPT" \
  || die "build-appex.sh: no PROVISIONING_PROFILE wiring"
cp_line=$(grep -n 'cp "\$PROVISIONING_PROFILE"' "$SCRIPT" | head -1 | cut -d: -f1)
sign_line=$(grep -n '^codesign ' "$SCRIPT" | head -1 | cut -d: -f1)
[[ -n "$cp_line" && -n "$sign_line" && "$cp_line" -lt "$sign_line" ]] \
  || die "build-appex.sh: profile embed must precede codesign"
refuse_line=$(grep -n 'PROVISIONING_PROFILE names no file' "$SCRIPT" | head -1 | cut -d: -f1)
swiftc_line=$(grep -n '"\$SWIFTC"' "$SCRIPT" | head -1 | cut -d: -f1)
[[ -n "$refuse_line" && -n "$swiftc_line" && "$refuse_line" -lt "$swiftc_line" ]] \
  || die "build-appex.sh: profile refusal must precede swiftc"
pass "appex profile embed precedes signing"

echo "entitlements: $PASS checks passed"
