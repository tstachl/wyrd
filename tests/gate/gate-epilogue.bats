#!/usr/bin/env bats
# tests/gate/gate-epilogue.bats — gate-path suite epilogue: no secrets in host logs.
#
# Ports the host-side leak check that used to end the legacy host
# orchestrator: scan every log the host wrote (guest logs
# are checked in-guest by each leg) plus the build logs beside
# $RUN, and fail if any credential material appears. Owner secrets
# always exist past phase 1; every later phase's credentials exist
# because the gate entry runs the full prefix unconditionally
# (no --step selection on the gate path yet). Runs as the build
# user under tests/gate/gate-entry.sh, last in the bats invocation.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB; do
    [ -n "${!v+x}" ] || { echo "gate-epilogue: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

@test "no secrets in host logs" {
  # Per-group scan: skip groups whose credential files are missing
  # or empty (an earlier phase failed before minting them — the phase
  # failure itself is the verdict). An empty secret would match every
  # non-empty log line, turning the epilogue into a misleading
  # secondary failure instead of preserving the original one.
  leak_scan() { # <identity-file> <passphrase-file>
    # Skip groups never minted (or only partially written) by a
    # failed earlier phase: an empty secret would match every log
    # line. Content-tested, not just -s: a whitespace-only partial
    # file still yields an empty secret after command substitution.
    [[ -n "$(cat "$1" 2>/dev/null)" && -n "$(cat "$2" 2>/dev/null)" ]] || return 0
    local f s
    for f in "$RUN"/logs/* "$STATE_DIR"/build-logs/*; do
      [[ -f "$f" ]] || continue
      for s in "$(cat "$1")" "$(cat "$2")"; do
        grep -qF "$s" "$f" && die "secret leaked into $(basename "$f")"
      done
    done
    return 0
  }
  leak_scan "$RUN/creds/owner/identity" "$RUN/creds/owner/passphrase"
  leak_scan "$RUN/creds/member-n/identity" "$RUN/creds/member-n/passphrase"
  leak_scan "$RUN/creds/headless-owner/identity" "$RUN/creds/headless-owner/passphrase"
  leak_scan "$RUN/creds/headless-c/identity" "$RUN/creds/headless-c/passphrase"
  leak_scan "$RUN/creds/headless-e/identity" "$RUN/creds/headless-e/passphrase"
  leak_scan "$RUN/creds/headless-d/identity" "$RUN/creds/headless-d/passphrase"
  leak_scan "$RUN/creds/crash-o/identity" "$RUN/creds/crash-o/passphrase"
  leak_scan "$RUN/creds/crash-n/identity" "$RUN/creds/crash-n/passphrase"
  leak_scan "$RUN/creds/exh/identity" "$RUN/creds/exh/passphrase"
  leak_scan "$RUN/creds/uparent-o/identity" "$RUN/creds/uparent-o/passphrase"
  leak_scan "$RUN/creds/uparent-n/identity" "$RUN/creds/uparent-n/passphrase"
}
