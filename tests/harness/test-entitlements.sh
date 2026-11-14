#!/usr/bin/env bash
# tests/harness/test-entitlements.sh — pin the production
# entitlements decision: minimal sandbox-plus-group sets on both
# targets, one App Group everywhere, and the appex provisioning
# wiring. Linux-safe: static assertions only (the real
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

APP="$ROOT/packaging/macos/Wyrd.entitlements"
APPEX="$ROOT/packaging/macos/fileprovider/WyrdFileProvider.entitlements"
GROUP='group.md.enesis.wyrd'

# 1. The decision is recorded and names both entitlement files.
grep -Fq 'Production entitlements and provisioning (decided)' \
  "$ROOT/packaging/macos/README.md" \
  || die "README: decision section missing"
grep -Fq 'packaging/macos/Wyrd.entitlements' "$ROOT/packaging/macos/README.md" \
  || die "README: app entitlements path missing"
pass "README pins the entitlements decision"

# 2. Both targets are sandboxed on the group, literal, no team prefix.
for f in "$APP" "$APPEX"; do
  grep -Fq '<key>com.apple.security.app-sandbox</key>' "$f" \
    || die "$f: sandbox key missing"
  grep -Fq "<string>$GROUP</string>" "$f" \
    || die "$f: missing <$GROUP>"
done
pass "app and appex share the sandbox plus the group"

# 3. Minimal by default: no other com.apple.* key on either target.
# Adding a capability (files, login items, devicecheck) must update
# this guard with its rationale, never slip in silently.
allowed="$(printf 'com.apple.security.app-sandbox\ncom.apple.security.application-groups')"
for f in "$APP" "$APPEX"; do
  keys="$(grep -o 'com\.apple[^<]*' "$f" | sort -u)"
  [[ "$keys" == "$allowed" ]] || die "$f: non-minimal entitlement keys: $keys"
done
pass "entitlement sets stay minimal"

# 4. The group agrees with the Swift sources on declaration lines.
grep -Fq "groupIdentifier = \"$GROUP\"" \
  "$ROOT/packaging/macos/fileprovider/Sources/SocketConfig.swift" \
  || die "SocketConfig.swift: declaration drifted"
grep -Fq "groupIdentifier = \"$GROUP\"" \
  "$ROOT/packaging/macos/fileprovider/Sources/SocketProbe.swift" \
  || die "SocketProbe.swift: declaration drifted"
pass "group agrees with the Swift declarations"

# 5. Appex provisioning wiring: opt-in embed, refusal on a missing
# path, embedded before signing (order: cp precedes codesign).
SCRIPT="$ROOT/packaging/macos/fileprovider/build-appex.sh"
grep -Fq 'PROVISIONING_PROFILE' "$SCRIPT" \
  || die "build-appex.sh: no PROVISIONING_PROFILE wiring"
grep -Fq 'embedded.provisionprofile' "$SCRIPT" \
  || die "build-appex.sh: no embedded.provisionprofile target"
cp_line=$(grep -n 'embedded.provisionprofile' "$SCRIPT" | head -1 | cut -d: -f1)
sign_line=$(grep -n '^codesign ' "$SCRIPT" | head -1 | cut -d: -f1)
[[ -n "$cp_line" && -n "$sign_line" && "$cp_line" -lt "$sign_line" ]] \
  || die "build-appex.sh: profile embed must precede codesign"
# Fail-fast: a missing profile refuses before the minutes-long
# swiftc, leaving no partial bundle behind.
refuse_line=$(grep -n 'PROVISIONING_PROFILE names no file' "$SCRIPT" | head -1 | cut -d: -f1)
swiftc_line=$(grep -n '"\$SWIFTC"' "$SCRIPT" | head -1 | cut -d: -f1)
[[ -n "$refuse_line" && -n "$swiftc_line" && "$refuse_line" -lt "$swiftc_line" ]] \
  || die "build-appex.sh: profile refusal must precede swiftc"
pass "appex profile embed precedes signing"

echo "entitlements: $PASS checks passed"
