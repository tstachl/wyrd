#!/usr/bin/env bash
# tests/harness/test-relay-forensics.sh — the relay guest stays
# diagnosable without a shell or a share (the 2026-10-08 ~30s relay
# outages left neither guest logs nor a host load record, so neither
# layer could be classified). Pure bash, no guest, no nix: run it
# anywhere with `bash tests/harness/test-relay-forensics.sh`.
#
# Post-mortem forensics ride the host now: the provisioner captures
# the instance console log and instance info at teardown, and the
# suite pulls guest logs on demand over the agent. So this pins the
# guest half the classification depends on: the relay runs the
# control-plane service with no share-dependent units (with no
# shared filesystem there is nowhere for a guest timer to publish
# to, and a guest-local dump the suite never pulls is write-only
# forensics), and nothing in nix/microvm/ references the retired
# state share. (The host availability logger used to live in the
# deleted dev-loop runner; the gate provisioner has no equivalent
# yet — that witness belongs to the gate repo, not here.)
# (Wired into the CI shell-tests job alongside the other harness stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RELAY_NIX="$SCRIPT_DIR/../../nix/microvm/relay.nix"
COMMON_NIX="$SCRIPT_DIR/../../nix/microvm/common.nix"

PASS=0

# The relay serves the control plane as a VM service.
grep -q 'systemd.services.nostr-relay' "$RELAY_NIX" \
  || { echo "FAIL: relay.nix has no relay service" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay service declared"
grep -q 'Restart.*on-failure' "$RELAY_NIX" \
  || { echo "FAIL: relay service does not restart on failure" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay service restarts on failure"

# No share-dependent units anywhere: the retired virtiofs mount the
# old journal timer ordered on is gone, and nothing may reference the
# old share path (a guest timer publishing nowhere is write-only
# forensics).
grep -rn '/mnt/wyrd-state' "$SCRIPT_DIR/../../nix/microvm/" \
  && { echo "FAIL: nix/microvm still references the retired state share" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: no state-share references in nix/microvm"
grep -q 'wyrd-ssh-key' "$RELAY_NIX" \
  && { echo "FAIL: relay.nix still mentions the retired ssh-key unit" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: no ssh-key unit in relay.nix"
grep -q 'wyrd-ssh-key\|services.openssh' "$COMMON_NIX" \
  && { echo "FAIL: common.nix still carries ssh infrastructure" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: no ssh infrastructure in common.nix"

echo "relay-forensics: $PASS checks passed"
