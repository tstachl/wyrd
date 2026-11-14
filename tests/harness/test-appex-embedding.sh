#!/usr/bin/env bash
# tests/harness/test-appex-embedding.sh — pin the appex embedding
# decision: the extension ships inside the app bundle, and the bundle
# identifiers plus the App Group agree across the four places that
# name them.
#
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-appex-embedding.sh`. (Wired into the CI
# shell-tests job alongside the other stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

# 1. The decision is recorded and names the embedded path.
grep -q 'Contents/PlugIns' "$ROOT/packaging/macos/README.md" \
  || die "README names no Contents/PlugIns path"
grep -q 'PlugIns/WyrdFileProvider.appex' "$ROOT/packaging/macos/README.md" \
  || die "README does not pin PlugIns/WyrdFileProvider.appex"
pass "README pins the embedded appex path"

# 2. One App Group everywhere, literal, no team prefix.
GROUP="group.md.enesis.wyrd"
for f in \
  "packaging/macos/fileprovider/WyrdFileProvider.entitlements" \
  "packaging/macos/fileprovider/Sources/SocketConfig.swift" \
  "packaging/macos/fileprovider/Sources/SocketProbe.swift"; do
  grep -q "$GROUP" "$ROOT/$f" || die "$f: missing $GROUP"
done
pass "app group agrees across entitlements and Swift sources"

# 3. Bundle identifiers nest: app md.enesis.wyrd, appex appends.
grep -q '"identifier": "md.enesis.wyrd"' "$ROOT/crates/wyrd-desktop/tauri.conf.json" \
  || die "tauri.conf.json: app identifier drifted"
grep -q '<string>md.enesis.wyrd.fileprovider</string>' "$ROOT/packaging/macos/fileprovider/Info.plist" \
  || die "Info.plist: appex identifier drifted"
pass "bundle identifiers nest (app plus .fileprovider)"

echo "appex-embedding: $PASS checks passed"
