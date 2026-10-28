#!/usr/bin/env bash
# nix/microvm/dev-as-guest.sh — dev-loop-only de-escalation wrapper.
#
# DEV LOOP ONLY. Never executed on Odin via the gate task path: the
# task runner runs the suite as the share owner already, so the suite
# invokes wyrd directly and this helper is not in its closure (pinned
# by tests/test-no-host-privilege.sh). The dev loop runs the suite as
# root, and the credential hardening refuses cross-uid opens both
# directions, so every host wyrd invocation drops to the guest
# owner's uid first. That uid is pinned (nix/microvm/common.nix)
# to the provision host's suite uid, so host and guest agree on
# file ownership across the shared state dir; setpriv works with
# numeric ids and needs no passwd entry, unlike su/runuser.
#
# Usage: dev-as-guest.sh <cmd> [args...]
# Provided to the suite as GATE_AS_GUEST_BIN by run-microvm.sh.
set -euo pipefail
exec setpriv --reuid 981 --regid 981 --clear-groups -- "$@"
