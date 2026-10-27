#!/usr/bin/env bats
# tests/gate-epilogue.bats — gate-path suite epilogue: no secrets in host logs.
#
# Ports the host-side leak check at the end of
# tests/alpha-microvm.sh: scan every log the host wrote (guest logs
# are checked in-guest by each leg) plus the build logs beside
# $RUN, and fail if any credential material appears. Owner secrets
# always exist past phase 1; every later phase's credentials exist
# because the gate entry runs the full prefix unconditionally
# (no --step selection on the gate path yet). Runs as the build
# user under tests/gate-entry.sh, last in the bats invocation.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB; do
    [ -n "${!v+x}" ] || { echo "gate-epilogue: $v is not set (run through tests/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  source "$BATS_TEST_DIRNAME/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate-entry.sh (gate task)" --check-owner-pid
}

@test "no secrets in host logs" {
  for f in "$RUN"/logs/* "$STATE_DIR"/build-logs/*; do
    [[ -f "$f" ]] || continue
    for s in "$(cat "$RUN/creds/owner/identity")" "$(cat "$RUN/creds/owner/passphrase")"; do
      grep -qF "$s" "$f" && die "secret leaked into $(basename "$f")"
    done
    for s in "$(cat "$RUN/creds/member-n/identity")" "$(cat "$RUN/creds/member-n/passphrase")"; do
      grep -qF "$s" "$f" && die "secret leaked into $(basename "$f")"
    done
    for s in "$(cat "$RUN/creds/headless-owner/identity")" "$(cat "$RUN/creds/headless-owner/passphrase")" \
             "$(cat "$RUN/creds/headless-c/identity")" "$(cat "$RUN/creds/headless-c/passphrase")" \
             "$(cat "$RUN/creds/headless-e/identity")" "$(cat "$RUN/creds/headless-e/passphrase")" \
             "$(cat "$RUN/creds/headless-d/identity")" "$(cat "$RUN/creds/headless-d/passphrase")"; do
      grep -qF "$s" "$f" && die "secret leaked into $(basename "$f")"
    done
    for s in "$(cat "$RUN/creds/crash-o/identity")" "$(cat "$RUN/creds/crash-o/passphrase")" \
             "$(cat "$RUN/creds/crash-n/identity")" "$(cat "$RUN/creds/crash-n/passphrase")"; do
      grep -qF "$s" "$f" && die "secret leaked into $(basename "$f")"
    done
    for s in "$(cat "$RUN/creds/exh/identity")" "$(cat "$RUN/creds/exh/passphrase")"; do
      grep -qF "$s" "$f" && die "secret leaked into $(basename "$f")"
    done
    for s in "$(cat "$RUN/creds/uparent-o/identity")" "$(cat "$RUN/creds/uparent-o/passphrase")" \
             "$(cat "$RUN/creds/uparent-n/identity")" "$(cat "$RUN/creds/uparent-n/passphrase")"; do
      grep -qF "$s" "$f" && die "secret leaked into $(basename "$f")"
    done
  done
}
