#!/usr/bin/env bash
# nix/microvm/dev-hand-to-guest.sh — dev-loop-only share handover.
#
# DEV LOOP ONLY. Never executed on Odin via the gate task path: there
# the suite runs as the share owner already, so created files arrive
# correctly owned and the handover is a no-op (pinned by
# tests/test-no-host-privilege.sh). The dev loop runs the suite as
# root while guests read as uid 1000, so credential material the suite
# creates must be handed over explicitly.
#
# Usage: dev-hand-to-guest.sh <dir>...
set -euo pipefail
exec chown -R 1000:1000 -- "$@"
