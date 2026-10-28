#!/usr/bin/env bash
# tests/test-topology-toml.sh — the [topology] declaration in gate.toml
# stays schema-valid and matches the microVM sources it describes:
# every box carries exactly the typed keys the gate task-topology
# parser requires, values have the parser's shapes, and the facts
# agree with nix/microvm/{relay,peer-o,peer-n}.nix. The parser itself
# is pinned by gate's own suite; this pins wyrd's declaration. Pure
# bash + grep, no guest, no nix.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

# Scope everything below to the [topology] section (it is the last
# section in gate.toml, so everything from its header on is topology).
TOPO="$(sed -n '/^\[topology\]/,$p' "$ROOT/gate.toml")"
[[ -n "$TOPO" ]] || die "gate.toml has no [topology] section"

printf '%s\n' "$TOPO" | grep -qx 'version = 1' \
  || die "topology version is not exactly 'version = 1'"
pass "topology version is 1"

# The declared box set is exactly the e2e trio, order-free.
DECLARED="$(printf '%s\n' "$TOPO" | grep -oE '^\[topology\.box\.[a-z0-9-]+\]' \
  | sed -E 's/^\[topology\.box\.//; s/\]$//' | sort -u)"
[[ "$(printf '%s\n' "$DECLARED" | tr '\n' ' ')" == "peer-n peer-o relay " ]] \
  || die "declared boxes are not exactly peer-o peer-n relay"
pass "declared boxes are exactly the e2e trio"
for _b in peer-o peer-n relay; do
  count="$(printf '%s\n' "$TOPO" | grep -cE "^\[topology\.box\.${_b}\]$")"
  [[ "$count" == 1 ]] || die "box $_b declared $count times"
done
unset _b count
pass "no box section is duplicated or missing"

# Per-box shape: exactly the six mandatory keys, typed values.
# Body of one box section (header and any following header excluded).
section_of() {
  sed -n "/^\\[topology\\.box\\.$1\\]/,/^\\[/p" <<< "$TOPO" | sed '1d' | grep -vE '^\[' || true
}
strip() { sed -E 's/^"//; s/"$//' <<< "$1"; }
for _b in peer-o peer-n relay; do
  BODY="$(section_of "$_b")"
  for _k in vcpu mem_mb tap mac ip ssh; do
    count="$(printf '%s\n' "$BODY" | grep -cE "^${_k} = " || true)"
    [[ "$count" == 1 ]] || die "box $_b key $_k appears $count times"
  done
  unset _k count
  EXTRA="$(printf '%s\n' "$BODY" | grep -vE '^(vcpu|mem_mb|tap|mac|ip|ssh|slug) = ' || true)"
  [[ -z "$EXTRA" ]] || die "box $_b has unknown keys:$EXTRA"
  VCPU="$(printf '%s\n' "$BODY" | sed -nE 's/^vcpu = //p')"
  MEM="$(printf '%s\n' "$BODY" | sed -nE 's/^mem_mb = //p')"
  [[ "$VCPU" =~ ^[0-9]+$ ]] && (( VCPU >= 1 && VCPU <= 64 )) \
    || die "box $_b vcpu '$VCPU' is not a bare 1-64 int"
  [[ "$MEM" =~ ^[0-9]+$ ]] && (( MEM >= 128 && MEM <= 65536 )) \
    || die "box $_b mem_mb '$MEM' is not a bare 128-65536 int"
  TAP="$(printf '%s\n' "$BODY" | sed -nE 's/^tap = //p')"
  [[ "$TAP" =~ ^\"[A-Za-z0-9_-]+\"$ ]] || die "box $_b tap '$TAP' is not quoted"
  MAC="$(printf '%s\n' "$BODY" | sed -nE 's/^mac = //p')"
  [[ "$MAC" =~ ^\"[0-9a-f]{2}(:[0-9a-f]{2}){5}\"$ ]] \
    || die "box $_b mac '$MAC' is not a quoted MAC"
  IP="$(printf '%s\n' "$BODY" | sed -nE 's/^ip = //p')"
  [[ "$IP" =~ ^\"[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+/[0-9]+\"$ ]] \
    || die "box $_b ip '$IP' is not quoted CIDR"
  SSH="$(printf '%s\n' "$BODY" | sed -nE 's/^ssh = //p')"
  [[ "$SSH" == true || "$SSH" == false ]] \
    || die "box $_b ssh '$SSH' is not a bare bool"
done
unset _b BODY EXTRA VCPU MEM TAP MAC IP SSH
pass "every box carries exactly the schema keys with typed values"

# The facts agree with the nix sources the provisioner builds from.
nix_of() { case "$1" in relay) echo relay.nix;; *) echo "$1.nix";; esac; }
for _b in peer-o peer-n relay; do
  NIX="$ROOT/nix/microvm/$(nix_of "$_b")"
  BODY="$(section_of "$_b")"
  VCPU="$(printf '%s\n' "$BODY" | sed -nE 's/^vcpu = //p')"
  MEM="$(printf '%s\n' "$BODY" | sed -nE 's/^mem_mb = //p')"
  TAP="$(strip "$(printf '%s\n' "$BODY" | sed -nE 's/^tap = //p')")"
  MAC="$(strip "$(printf '%s\n' "$BODY" | sed -nE 's/^mac = //p')")"
  ADDR="$(strip "$(printf '%s\n' "$BODY" | sed -nE 's/^ip = //p')" | cut -d/ -f1)"
  grep -qE "vcpu = ${VCPU};" "$NIX" || die "box $_b vcpu disagrees with $NIX"
  grep -qE "mem = ${MEM};" "$NIX" || die "box $_b mem_mb disagrees with $NIX"
  grep -qE "id = \"${TAP}\";" "$NIX" || die "box $_b tap disagrees with $NIX"
  grep -qE "mac = \"${MAC}\";" "$NIX" || die "box $_b mac disagrees with $NIX"
  grep -qE "address = \"${ADDR}\";" "$NIX" || die "box $_b ip disagrees with $NIX"
done
unset _b NIX BODY VCPU MEM TAP MAC ADDR
pass "declared facts match the nix sources"

echo "topology-toml: $PASS checks passed"
