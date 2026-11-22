#!/usr/bin/env bash
# tests/harness/test-switchover.sh — pin the registry-owned File Provider
# switchover: production binds the group socket with no env vars, the
# dev env pair remains dev-only, and the docs agree.
#
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-switchover.sh`. (Wired into the CI
# shell-tests job alongside the other stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

# Socket binding/derivation lives in `server/socket.rs`; the production
# auto-bind call sits in `server/service.rs` beside the service lifecycle.
SERVER="$ROOT/crates/wyrd-daemon/src/owner/server/socket.rs"
BIND="$ROOT/crates/wyrd-daemon/src/owner/server/service.rs"
MOD="$ROOT/crates/wyrd-daemon/src/owner/mod.rs"
SWIFT="$ROOT/packaging/macos/fileprovider/Sources/SocketConfig.swift"
README="$ROOT/packaging/macos/README.md"
CONTRACT="$ROOT/docs/contracts/owner-interface.md"

# 1. Production bind exists: platform container derivation plus the
# served-drive auto-bind in bind_for_composer, with no env fallback
# in the production path.
grep -Fq 'FILEPROVIDER_GROUP_ID' "$SERVER" \
  || die "server: production group id missing"
grep -Fq 'default_group_container_with_home' "$SERVER" \
  || die "server: production container derivation missing"
grep -Fq 'production_group_container()' "$SERVER" \
  || die "server: production container existence gate missing"
grep -Fq 'None => match production_group_container()' "$BIND" \
  || die "server: production auto-bind missing from bind_for_composer"
pass "server binds the group socket with no env vars in production"

# 2. The dev env pair remains, marked dev-only — never removed, never
# promoted to production.
grep -Fq 'FILEPROVIDER_GROUP_CONTAINER_ENV' "$SERVER" \
  || die "server: dev container env missing"
grep -Fq 'FILEPROVIDER_DRIVE_ID_ENV' "$SERVER" \
  || die "server: dev drive env missing"
grep -Fq 'Dev-only' "$SERVER" \
  || die "server: dev bootstrap is not marked dev-only"
grep -Fq 'never consults this' "$SERVER" \
  || die "server: production path must not consult the dev env"
pass "dev env bootstrap stays dev-only"

# 3. The group id agrees across the daemon and Swift declaration lines.
GROUP='group.md.enesis.wyrd'
grep -Fq "\"$GROUP\"" "$SERVER" \
  || die "server: group id drifted"
grep -Fq "groupIdentifier = \"$GROUP\"" "$SWIFT" \
  || die "SocketConfig.swift: group identifier drifted"
grep -Fq 'FILEPROVIDER_GROUP_ID' "$MOD" \
  || die "owner mod: group id not re-exported"
pass "group id agrees daemon-side and Swift-side"

# 4. The Swift domain parser stays registry-owned: drive hex only, no
# env-var fallback, dev override stays harness-only.
grep -Fq 'Registry-owned production' "$SWIFT" \
  || die "SocketConfig.swift: production comment missing"
grep -Fq 'no env-var fallback' "$SWIFT" \
  || die "SocketConfig.swift: no-env-fallback not stated"
pass "Swift domain resolves from the registry with no env fallback"

# 5. The normative transport documents the daemon-half production
# bind plus the pending domain half and the dev-only override.
grep -Fq 'daemon half of the registry-owned' "$CONTRACT" \
  || die "contract: daemon-half production missing"
grep -Fq 'full registry-to-domain association' "$CONTRACT" \
  || die "contract: pending domain half not stated"
grep -Fqi 'dev-only env pair' "$CONTRACT" \
  || die "contract: dev-only override not stated"
pass "contract documents daemon-half production plus pending domain half"

# 6. The packaging note records the switchover acceptance: registered
# drive in Finder with env unset, reboot survival, main-window Available —
# scoped to the daemon half with the signed-container check pending.
grep -Fq 'Registry-owned switchover (production daemon half)' "$README" \
  || die "README: switchover section missing"
grep -Fq 'with no env' "$README" \
  || die "README: no-env acceptance missing"
grep -Fq 'survives' "$README" \
  || die "README: reboot survival missing"
grep -Fq 'verified for development signing only' "$README" \
  || die "README: signed-container scope missing"
grep -Fq 'daemon half of the' "$README" \
  || die "README: scheme section not scoped to daemon half"
grep -Fq 'domain half' "$README" \
  || die "README: pending domain half not stated"
pass "README records the daemon-half acceptance with pending check"

# 7. Production derivation pins: home-based path math, the is_dir
# decoy gate, and HOME isolation for owner tests.
grep -Fq 'production_container_derives_from_home' \
  "$ROOT/crates/wyrd-daemon/src/owner/tests_owner.rs" \
  || die "daemon tests: production derivation pin missing"
grep -Fq 'production_group_container_with_home' \
  "$ROOT/crates/wyrd-daemon/src/owner/tests_owner.rs" \
  || die "daemon tests: is_dir gate pin missing"
grep -Fq 'HomeGuard::scratch_without_container' \
  "$ROOT/crates/wyrd-daemon/src/owner/tests_owner.rs" \
  || die "daemon tests: HOME isolation missing"
pass "daemon unit tests pin derivation, decoy gate, and isolation"

echo "switchover: $PASS checks passed"
