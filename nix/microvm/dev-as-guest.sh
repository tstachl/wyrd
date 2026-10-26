#!/usr/bin/env bash
# nix/microvm/dev-as-guest.sh — dev-loop-only de-escalation wrapper.
#
# DEV LOOP ONLY. Never executed on Odin via the gate task path: the
# task runner runs the suite as the share owner already, so the suite
# invokes wyrd directly and this helper is not in its closure (pinned
# by tests/test-no-host-privilege.sh). The dev loop runs the suite as
# root, and the credential hardening refuses cross-uid opens both
# directions, so every host wyrd invocation drops to uid 1000 first.
# setpriv works with numeric ids and needs no passwd entry, unlike
# su/runuser.
#
# Usage: dev-as-guest.sh <cmd> [args...]
# Provided to the suite as GATE_AS_GUEST_BIN by run-microvm.sh.
set -euo pipefail
exec setpriv --reuid 1000 --regid 1000 --clear-groups -- "$@"
