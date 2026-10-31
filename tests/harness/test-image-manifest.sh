#!/usr/bin/env bash
# tests/harness/test-image-manifest.sh — the gate image declaration stays
# schema-valid and single-sourced: gate.toml names image builds (never a
# topology section), each role's gate.box facts validate, and the nix
# sources agree with the declared builds. The gate-owned guest
# module derives manifest.json from the same gate.box options, so
# drift is impossible by construction — this test pins the
# declaration side (what the host builds and what the suite dials),
# while the derivation re-validates the manifest fail-closed at
# build time and the gateManifest eval below asserts the exact wire
# bytes. Pure bash + grep, with optional nix eval cross-checks when
# nix is available.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

FIXTURE="$(mktemp -d)"
trap 'rm -rf "$FIXTURE"' EXIT

# The retired qemu backend's declaration must be gone: a
# [tasks.*.topology] section refuses at parse time on the host.
grep -qE '^\[tasks\.[^]]*\.topology' "$ROOT/gate.toml" \
  && die "gate.toml still declares a task topology section (refused by the Incus backend)"
pass "no task topology section in gate.toml"

# The task builds are exactly the three images, role-addressed.
BUILDS="$(sed -n '/^\[tasks\.wyrd-gate-e2e\.builds\]/,/^\[/p' "$ROOT/gate.toml" | grep -E '^[a-z0-9_-]+ = ' || true)"
[[ "$(printf '%s\n' "$BUILDS" | sort | tr '\n' ' ')" == 'img-peer-n = "images.peer-n" img-peer-o = "images.peer-o" img-relay = "images.relay" ' ]] \
  || die "task builds are not exactly the three images:$BUILDS"
pass "task builds are exactly the three images"
grep -q '^relayRole = "relay"$' "$ROOT/gate.toml" \
  || die "relayRole does not name the relay manifest role"
pass "relayRole names the relay manifest role"
grep -q '"jq"' "$ROOT/gate.toml" \
  || die "requires does not include jq (the entry parses manifests with it)"
pass "jq in task requires"

# Per-role facts in the nix sources: typed values matching the v1
# contract the host enforces (kind is fixed by the derivation;
# prefix membership is checked at launch against host policy).
nix_of() { case "$1" in relay) echo relay.nix;; *) echo "$1.nix";; esac; }
for _b in peer-o peer-n relay; do
  NIX="$ROOT/nix/microvm/$(nix_of "$_b")"
  ROLE="$(grep -E '^\s*role = ' "$NIX" | sed -E 's/.*role = "//; s/".*//')"
  [[ "$ROLE" == "$_b" ]] || die "box $_b role '$ROLE' does not name the role"
  VCPU="$(grep -E '^\s*vcpu = ' "$NIX" | grep -oE '[0-9]+')"
  [[ "$VCPU" =~ ^[0-9]+$ ]] && (( VCPU >= 1 && VCPU <= 64 )) \
    || die "box $_b vcpu '$VCPU' is not a bare 1-64 int"
  MEM="$(grep -E '^\s*memMb = ' "$NIX" | grep -oE '[0-9]+')"
  [[ "$MEM" =~ ^[0-9]+$ ]] && (( MEM >= 512 && MEM <= 131072 )) \
    || die "box $_b memMb '$MEM' is not a bare 512-131072 int"
  MAC="$(grep -E '^\s*mac = ' "$NIX" | sed -E 's/.*mac = "//; s/".*//')"
  [[ "$MAC" =~ ^([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$ ]] \
    || die "box $_b mac '$MAC' is not six hex octets"
  IP="$(grep -E '^\s*ip = ' "$NIX" | sed -E 's/.*ip = "//; s/".*//')"
  [[ "$IP" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] \
    || die "box $_b ip '$IP' is not a bare IPv4 address"
  for _o in ${IP//./ }; do
    [ "$_o" -le 255 ] || die "box $_b ip octet out of bounds: $IP"
  done
  unset _o
done
unset _b NIX ROLE VCPU MEM MAC IP
pass "every role carries typed gate.box facts"
# The declared facts are the suite's addressing: peer pair plus relay
# on the test bridge.
grep -q 'ip = "10.0.7.11"' "$ROOT/nix/microvm/peer-o.nix" || die "peer-o is not .11"
grep -q 'ip = "10.0.7.12"' "$ROOT/nix/microvm/peer-n.nix" || die "peer-n is not .12"
grep -q 'ip = "10.0.7.10"' "$ROOT/nix/microvm/relay.nix" || die "relay is not .10"
pass "declared facts match the test bridge addressing"

# The flake exposes one output per role whose dir holds exactly the
# manifest and the tarball, and no microvm input remains. (The
# nix/microvm/ directory path and the empty-src placeholder stay by
# design; only the flake input and its module wiring go.)
grep -qE 'microvm\.(url|inputs)|microvm\.nixosModules|microvmGuest' "$ROOT/flake.nix" \
  && die "flake.nix still references the retired microvm input"
pass "no microvm input in flake.nix"
grep -q "config\.system\.build\.gateImage" "$ROOT/flake.nix" \
  || die "flake.nix exposes no gateImage outputs"
grep -q 'incus-virtual-machine.nix' "$ROOT/flake.nix" \
  || die "guests are not built on the incus virtual-machine module"
grep -q 'gate\.nixosModules\.guest' "$ROOT/flake.nix" \
  || die "guests are not built on the gate-owned guest module"
pass "flake exposes gateImage outputs on the incus and gate modules"

# Manifest content, evaluated without building: system.build.gateManifest
# is the pure passthrough of the manifest the image carries, so this
# asserts the exact wire bytes (kind, key set, typed fields) at eval
# time. jq enforces the v1 shape the host enforces at launch.
if command -v nix >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
  for _b in peer-o peer-n relay; do
    if ! manifest_json="$(nix eval --json ".#nixosConfigurations.wyrd-${_b}.config.system.build.gateManifest" 2>"$FIXTURE/nix-err-$_b.txt")"; then
      echo "gateManifest eval failed for $_b:" >&2
      cat "$FIXTURE/nix-err-$_b.txt" >&2
      die "gateManifest for $_b does not evaluate"
    fi
    echo "$manifest_json" | jq -e '.kind == "gate.image/v1"' >/dev/null       || die "manifest kind is not gate.image/v1 for $_b"
    echo "$manifest_json" | jq -e '([keys_unsorted[]] | sort) == ["ip","kind","mac","memMb","role","shares","vcpu"]' >/dev/null       || die "manifest key set is wrong for $_b"
    echo "$manifest_json" | jq -e --arg role "$_b" '.role == $role and (.vcpu | type) == "number" and (.memMb | type) == "number"' >/dev/null       || die "manifest role/vcpu/memMb mistyped for $_b"
    echo "$manifest_json" | jq -e '.shares == []' >/dev/null       || die "manifest shares are not empty for $_b"
  done
  unset _b manifest_json
  pass "evaluated gateManifest carries the exact v1 wire bytes"
else
  echo "  SKIP: nix or jq unavailable, manifest content check omitted"
fi

# Eval cross-check: the evaluated gate.box must equal what the entry
# would read. Eval failures surface with the nix error, not a bare
# refusal.
if command -v nix >/dev/null 2>&1; then
  for _b in peer-o peer-n relay; do
    if ! role_json="$(nix eval --json ".#nixosConfigurations.wyrd-${_b}.config.gate.box" 2>"$FIXTURE/nix-err-$_b.txt")"; then
      echo "gate.box eval failed for $_b:" >&2
      cat "$FIXTURE/nix-err-$_b.txt" >&2
      die "gate.box for $_b does not evaluate"
    fi
    [[ -n "$role_json" ]] || die "gate.box for $_b evaluated empty"
    echo "$role_json" | grep -q "\"role\":\"$_b\"" || die "evaluated role for $_b mismatches"
  done
  unset _b role_json
  pass "evaluated gate.box matches the declared roles"
else
  echo "  SKIP: nix unavailable, eval cross-check omitted"
fi

echo "image-manifest: $PASS checks passed"
