#!/usr/bin/env bash
# tests/harness/test-live-mount-decl.sh — the Odin leg for the two
# ignored live-mount libtests stays wired end to end: task declaration,
# flake package and peer-o image, entry/leg guard, and the libtest
# target the docs name. Pure bash + grep: no guest, no nix, no run.
# Run from the checkout: bash tests/harness/test-live-mount-decl.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

# The task is data-only, like the other gate tasks: no topology, one
# image build, no relay role (mount evidence, not convergence).
TASK="$(sed -n '/^\[tasks\.live-mount\]/,/^$/p' "$ROOT/gate.toml")"
[[ -n "$TASK" ]] || die "gate.toml declares no live-mount task"
printf '%s\n' "$TASK" | grep >/dev/null '^entryPoint = "tests/gate/live-mount-entry.sh"$' \
  || die "task entryPoint is not the live-mount entry"
printf '%s\n' "$TASK" | grep >/dev/null '"bats"' || die "bats missing from task requires"
printf '%s\n' "$TASK" | grep >/dev/null '"jq"' || die "jq missing from task requires"
printf '%s\n' "$TASK" | grep >/dev/null '^guest_log_dir = "wyrd-e2e-live-mount"$' \
  || die "guest log dir is not declared (evidence would not be fetched)"
printf '%s\n' "$TASK" | grep >/dev/null '^relayRole = ' \
  && die "live-mount task declares a relayRole (it is a mount evidence leg)"
BUILDS="$(sed -n '/^\[tasks\.live-mount\.builds\]/,/^\[/p' "$ROOT/gate.toml" | grep -E '^[a-z0-9_-]+ = ' || true)"
[[ "$(printf '%s\n' "$BUILDS" | sort | tr '\n' ' ')" == 'img-peer-o = "images.peer-o" ' ]] \
  || die "live-mount task builds are not exactly peer-o: $BUILDS"
pass "task declaration: entry, requires, log dir, one image build, no relay"

# The harness is a flake package baked into the peer-o image; the
# guest needs no toolchain.
grep >/dev/null 'wyrd-live-mount-tests = liveMountTestsFor system' "$ROOT/flake.nix" \
  || die "flake exposes no live-mount libtest package"
grep >/dev/null 'role == "peer-o"' "$ROOT/flake.nix" \
  || die "peer-o image does not gate the libtest package on its role"
grep >/dev/null 'self.packages.x86_64-linux.wyrd-live-mount-tests' "$ROOT/flake.nix" \
  || die "peer-o image does not carry the libtest package"
grep >/dev/null 'cargo test -p wyrd-cli --lib --no-run' "$ROOT/flake.nix" \
  || die "libtest derivation does not build the library test target"
pass "flake builds the libtest harness and bakes it into peer-o"

# Entry and leg: wrapper-only guard, single process, both tests, and
# an explicit assertion that two tests actually ran.
ENTRY="$ROOT/tests/gate/live-mount-entry.sh"
LEG="$ROOT/tests/gate/live-mount.bats"
[ -f "$ENTRY" ] && [ -x "$ENTRY" ] || die "live-mount entry is missing or not executable"
[ -f "$LEG" ] || die "live-mount leg is missing"
grep >/dev/null 'e2e_require_wrapped' "$LEG" \
  || die "live-mount leg has no wrapper guard"
grep >/dev/null -- '--ignored live_mount_' "$LEG" \
  || die "live-mount leg does not filter the two ignored tests"
grep >/dev/null 'running 2 tests' "$LEG" \
  || die "live-mount leg does not assert that both tests ran"
grep >/dev/null 'test result: ok. 2 passed' "$LEG" \
  || die "live-mount leg does not assert two passes"
grep >/dev/null 'guest_timeout 300' "$LEG" \
  || die "libtest call is not bounded"
grep >/dev/null '127.0.0.1' "$LEG" \
  || die "live-mount leg does not preflight loopback networking"
grep >/dev/null 'environment, not product' "$LEG" \
  || die "loopback preflight does not name an environment failure"
grep >/dev/null 'test-live-mount-decl.sh' "$ROOT/tests/harness/test-gate-entry.sh" \
  || die "gate-entry pin does not point at the dedicated live-mount pin"
pass "entry and leg are guarded, bounded, preflighted and non-vacuous"

# The documented command must name the library target; --bin wyrd
# would run zero tests and report a vacuous green.
TESTS="$ROOT/crates/wyrd-cli/src/tests_mount.rs"
grep >/dev/null 'fn live_mount_serves_read_write_until_shutdown' "$TESTS" \
  || die "first ignored live-mount test is missing"
grep >/dev/null 'fn live_mount_preserves_dirty_handle_across_shutdown' "$TESTS" \
  || die "second ignored live-mount test is missing"
grep >/dev/null -- '--bin wyrd' "$TESTS" \
  && die "test docs still name --bin wyrd (zero tests would run)"
grep >/dev/null -- '--lib' "$TESTS" || die "test docs do not name the library target"
pass "test docs name the libtest target"

echo "live-mount-decl: $PASS checks passed"
