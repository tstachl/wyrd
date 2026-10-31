#!/usr/bin/env bats
# tests/gate/gate-epilogue.bats — gate-path suite epilogue: no secrets in logs.
#
# Ports the host-side leak check that used to end the legacy host
# orchestrator: pull the guest logs and the credential material over
# the agent, then scan every log the suite wrote (guest legs check
# their own logs in-guest per leg; this is the second layer) plus
# the build logs beside $RUN, and fail if any credential material
# appears. Owner secrets always exist past phase 1; every later
# phase's credentials exist because the gate entry runs the full
# prefix unconditionally (no --step selection on the gate path yet).
# Runs as the build user under tests/gate/gate-entry.sh, last in the
# bats invocation.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN; do
    [ -n "${!v+x}" ] || { echo "gate-epilogue: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  load "$GATE_BATS_LIB/guest"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

# pull_group <role> <group>: fetch one credential pair into the
# host-side leak-scan tree. Warn-only: a failed earlier phase mints
# nothing, and the phase failure itself is the verdict — the scan
# skips unminted groups below.
pull_group() { # <role> <group>
  local role="$1" group="$2"
  mkdir -p "$RUN/leakscan/$group"
  guest_pull "$role" "$GUEST_RUN/creds/$group/identity" "$RUN/leakscan/$group/identity" 2>/dev/null \
    || return 0
  guest_pull "$role" "$GUEST_RUN/creds/$group/passphrase" "$RUN/leakscan/$group/passphrase" 2>/dev/null \
    || return 0
}

@test "no secrets in logs" {
  # Guest logs ride the agent into the host tree first, so the scan
  # below covers them. Best-effort: a dead guest loses its logs but
  # must not mask the phase failure that killed it.
  guest_pull peer-o "$GUEST_RUN/logs" "$RUN/logs/peer-o" 2>/dev/null \
    || echo "# warn: peer-o log pull failed" >&2
  guest_pull peer-n "$GUEST_RUN/logs" "$RUN/logs/peer-n" 2>/dev/null \
    || echo "# warn: peer-n log pull failed" >&2
  # Per-group scan: skip groups never minted (or only partially
  # written) by a failed earlier phase: an empty secret would match
  # every non-empty log line, turning the epilogue into a misleading
  # secondary failure instead of preserving the original one.
  pull_group peer-o owner
  pull_group peer-n member-n
  pull_group peer-o headless-owner
  pull_group peer-n headless-c
  pull_group peer-n headless-e
  pull_group peer-n headless-d
  pull_group peer-o crash-o
  pull_group peer-n crash-n
  pull_group peer-o exh
  pull_group peer-o uparent-o
  pull_group peer-n uparent-n
  leak_scan() { # <identity-file> <passphrase-file>
    # Skip groups never minted (or only partially written) by a
    # failed earlier phase: an empty secret would match every log
    # line. Content-tested, not just -s: a whitespace-only partial
    # file still yields an empty secret after command substitution.
    [[ -n "$(cat "$1" 2>/dev/null)" && -n "$(cat "$2" 2>/dev/null)" ]] || return 0
    local f s
    for f in "$RUN"/logs/* "$RUN"/logs/peer-o/* "$RUN"/logs/peer-n/* "$STATE_DIR"/build-logs/*; do
      [[ -f "$f" ]] || continue
      for s in "$(cat "$1")" "$(cat "$2")"; do
        grep -qF "$s" "$f" && die "secret leaked into ${f#$RUN/}"
      done
    done
    return 0
  }
  for g in owner member-n headless-owner headless-c headless-e headless-d crash-o crash-n exh uparent-o uparent-n; do
    leak_scan "$RUN/leakscan/$g/identity" "$RUN/leakscan/$g/passphrase"
  done
}
