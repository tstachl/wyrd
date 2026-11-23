#!/usr/bin/env bash
# tests/harness/test-appex-embedding.sh — pin the appex embedding
# decision: the extension ships inside the app bundle, and the bundle
# identifiers plus the App Group agree across the places that name
# them.
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

# 1. The accepted decision sentence plus the tree block, verbatim:
# a sidecar decision that merely names the rejected path must not pass.
grep -F >/dev/null 'The File Provider extension ships embedded in the app bundle:' \
  "$ROOT/packaging/macos/README.md" \
  || die "README: accepted decision sentence missing"
grep -F >/dev/null 'PlugIns/WyrdFileProvider.appex/' "$ROOT/packaging/macos/README.md" \
  || die "README: tree block missing PlugIns/WyrdFileProvider.appex/"
pass "README pins the accepted embedded layout"

# 2. One App Group everywhere, literal, on the declaration lines —
# a comment mentioning the group must not satisfy the check.
GROUP='group.md.enesis.wyrd'
grep -F >/dev/null "<string>$GROUP</string>" \
  "$ROOT/packaging/macos/fileprovider/WyrdFileProvider.entitlements" \
  || die "entitlements: missing <$GROUP>"
grep -F >/dev/null "groupIdentifier = \"$GROUP\"" \
  "$ROOT/packaging/macos/fileprovider/Sources/SocketConfig.swift" \
  || die "SocketConfig.swift: declaration drifted"
grep -F >/dev/null "groupIdentifier = \"$GROUP\"" \
  "$ROOT/packaging/macos/fileprovider/Sources/SocketProbe.swift" \
  || die "SocketProbe.swift: declaration drifted"
pass "app group agrees on its declaration lines"

# 3. Bundle identifiers nest: app md.enesis.wyrd, appex appends —
# and the build script signs the same appex identifier it ships.
# Product metadata rides the same config: the product name matches
# the Wyrd.app the install script builds, and the version is the
# single source the stage hook stamps on the appex.
grep -F >/dev/null '"identifier": "md.enesis.wyrd"' "$ROOT/crates/wyrd-desktop/tauri.conf.json" \
  || die "tauri.conf.json: app identifier drifted"
grep -F >/dev/null '"productName": "Wyrd"' "$ROOT/crates/wyrd-desktop/tauri.conf.json" \
  || die "tauri.conf.json: product name drifted"
grep -F >/dev/null '"version": "0.4.0"' "$ROOT/crates/wyrd-desktop/tauri.conf.json" \
  || die "tauri.conf.json: app version drifted"
grep -F >/dev/null '<string>md.enesis.wyrd.fileprovider</string>' \
  "$ROOT/packaging/macos/fileprovider/Info.plist" \
  || die "Info.plist: appex identifier drifted"
grep -F >/dev/null -- '--identifier md.enesis.wyrd.fileprovider' \
  "$ROOT/packaging/macos/fileprovider/build-appex.sh" \
  || die "build-appex.sh: codesign identifier drifted"
grep -F >/dev/null '"PlugIns/WyrdFileProvider.appex"' \
  "$ROOT/crates/wyrd-desktop/tauri.conf.json" \
  || die "tauri.conf.json: appex bundle mapping missing"
grep -F >/dev/null -- '--identifier md.enesis.wyrd' \
  "$ROOT/packaging/macos/domain-registrar/build-registrar.sh" \
  || die "build-registrar.sh: helper must sign as the containing app"
grep -F >/dev/null "<string>$GROUP</string>" \
  "$ROOT/packaging/macos/Wyrd.app.entitlements" \
  || die "app entitlements: missing <$GROUP>"
grep -F >/dev/null '"entitlements": "../../packaging/macos/Wyrd.app.entitlements"' \
  "$ROOT/crates/wyrd-desktop/tauri.conf.json" \
  || die "tauri.conf.json: app entitlements are not wired into the bundle"
grep -F >/dev/null -- '-emit-executable' \
  "$ROOT/packaging/macos/fileprovider/build-appex.sh" \
  || die "build-appex.sh: a Mach-O bundle would silently drop the App Group entitlement"
grep -F >/dev/null 'group_label_path' \
  "$ROOT/crates/wyrd-daemon/src/owner/server/socket.rs" \
  || die "socket.rs: no published drive-name label"
grep -F >/dev/null '.label' \
  "$ROOT/packaging/macos/domain-registrar/main.swift" \
  || die "registrar: drive-name label is not read"
grep -F >/dev/null 'socketResponds' \
  "$ROOT/packaging/macos/domain-registrar/main.swift" \
  || die "registrar: dead owners are not detected"
pass "MH_EXECUTE appex and registry-name label are pinned"
grep -F >/dev/null 'app-id: md.enesis.wyrd' \
  "$ROOT/packaging/linux/md.enesis.wyrd.yaml" \
  || die "linux manifest: app-id drifted"
pass "bundle identifiers nest, build script agrees"

# 4. The ad-hoc load refusal is recorded, not assumed: the README
# names the verdict on the ad-hoc appex and the team-signed deltas
# that unblock distribution, and the notarized check stays with
# the signing owner.
grep -F >/dev/null 'Ad-hoc `fileproviderd` load check' "$ROOT/packaging/macos/README.md" \
  || die "README: ad-hoc load check not recorded"
grep -F >/dev/null 'does not load, by mechanism' "$ROOT/packaging/macos/README.md" \
  || die "README: ad-hoc verdict missing"
grep -F >/dev/null '`fileproviderd` load check itself stays' "$ROOT/packaging/macos/README.md" \
  || die "README: notarized check ownership missing"
grep -F >/dev/null 'with the signing owner' "$ROOT/packaging/macos/README.md" \
  || die "README: signing owner missing"
grep -F >/dev/null 'no effective group-container entitlement' "$ROOT/packaging/macos/README.md" \
  || die "README: effective-entitlement mechanism missing"
pass "ad-hoc refusal recorded with team-signed deltas"

echo "appex-embedding: $PASS checks passed"
