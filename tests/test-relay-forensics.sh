#!/usr/bin/env bash
# tests/test-relay-forensics.sh — stub suite for the relay-flap
# forensics wiring (the 2026-10-08 ~30s relay outages left neither
# guest logs nor a host load record, so neither layer could be
# classified). Pure bash, no guest, no nix: run it anywhere with
# `bash tests/test-relay-forensics.sh`. It pins the guest half the
# classification depends on: the relay guest's journal timer in
# nix/microvm/relay.nix. (The host availability logger used to live
# in the deleted dev-loop runner; the gate provisioner has no
# equivalent yet — that witness belongs to the gate repo, not here.)
# (Wired into the CI shell-tests job alongside the other harness stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RELAY_NIX="$SCRIPT_DIR/../nix/microvm/relay.nix"

PASS=0

# The relay guest dumps its current-boot journal into the state share
# on a timer: teardown destroys the VM, and the relay carries no ssh
# key by design, so this dump is the only post-mortem path.
grep -q 'wyrd-relay-journal' "$RELAY_NIX" \
  || { echo "FAIL: relay.nix has no journal persistence unit" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay journal unit declared"
grep -q 'OnUnitActiveSec' "$RELAY_NIX" \
  || { echo "FAIL: relay journal unit is not on a repeating timer" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay journal dump repeats on a timer"
grep -q 'journalctl -b' "$RELAY_NIX" \
  || { echo "FAIL: relay journal dump does not capture the current boot" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay journal dump captures the current boot"
grep -q 'relay-journal.log' "$RELAY_NIX" \
  || { echo "FAIL: relay journal dump target path missing" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay journal dump lands in the run logs"
# The destination must be the host-visible run log dir, not just a
# file with the right name: a one-segment change to the share path
# keeps every name assertion green while writing where no run reads.
grep -q '/mnt/wyrd-state/run/logs/relay-journal.log' "$RELAY_NIX" \
  || { echo "FAIL: relay journal dump is not aimed at the host-visible run logs" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay journal dump aims at the host-visible run logs"
# The 60s freshness bound needs AccuracySec: systemd's default 1min
# scheduling slop would otherwise make the worst-case gap ~2min.
grep -q 'AccuracySec' "$RELAY_NIX" \
  || { echo "FAIL: relay journal timer has no accuracy bound" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay journal timer bounds its scheduling slop"
# Ordered on the share mount, not merely timed: an early tick before
# the virtiofs mount lands writes onto the guest-local root, where
# the mount then hides it — failing invisibly in exactly the way the
# dump exists to fix.
grep -q 'mnt-wyrd' "$RELAY_NIX" \
  || { echo "FAIL: relay journal unit is not ordered on the state-share mount" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay journal unit waits for the state-share mount"
# The heartbeat must mean a successful dump: staged through a temp
# file and renamed on success, so a failed journalctl can never
# leave a fresh heartbeat over a truncated log. The read is bounded
# too — a wedged journalctl stalls the oneshot and systemd skips
# timer elapses while it runs, silently ending the dump series.
grep -q '\.relay-journal\.tmp' "$RELAY_NIX" \
  || { echo "FAIL: relay journal dump is not staged through a temp file" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay journal dump stages through a temp file"
grep -q 'TimeoutStartSec' "$RELAY_NIX" \
  || { echo "FAIL: relay journal unit has no read bound" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay journal read is bounded"
# The ordering is the property, not the two files: the heartbeat
# must be the last && segment of the dump chain, so moving the date
# line back to the front (the exact R2 regression) fails even
# though both filenames are still present.
LAST_SEG="$(sed -n '/wyrd-relay-journal = {/,/};/p' "$RELAY_NIX" | tr '\n' ' ' | sed 's/.*&& //')"
printf '%s\n' "$LAST_SEG" | grep -q 'relay-journal\.ts' \
  || { echo "FAIL: relay heartbeat is not written after the dump" >&2; exit 1; }
PASS=$((PASS + 1))
echo "  PASS: relay heartbeat is written after the dump"

echo "relay-forensics: $PASS checks passed"
