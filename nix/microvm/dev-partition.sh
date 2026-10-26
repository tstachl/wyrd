#!/usr/bin/env bash
# nix/microvm/dev-partition.sh — dev-loop-only partition primitive.
#
# DEV LOOP ONLY. Never executed on Odin via the gate task path: there
# the host provides GATE_PARTITION_BIN as a host-owned binary over the
# task's declared taps, and this helper is not in the suite's closure
# (pinned by tests/test-no-host-privilege.sh). The dev loop runs as
# root on its own machine (its own trust domain) and performs the tap
# surgery directly.
#
# Usage: dev-partition.sh <tap> down|up
set -euo pipefail
tap="$1" how="$2"
[[ "$how" == down || "$how" == up ]] || { echo "  FAIL: dev-partition.sh takes down|up, not $how" >&2; exit 2; }
exec ip link set "$tap" "$how"
