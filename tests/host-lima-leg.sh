#!/usr/bin/env bash
# tests/host-lima-leg.sh — the desktop-host order-evidence leg. Runs INSIDE
# the guest (via lima/run-alpha.sh --host-leg); every assertion here is the
# leg. The existing alpha suite cannot run unchanged against the host (it
# asserts mount artifacts: mount.log truncation, FUSE mountpoints, serving
# lines), so this leg asserts the host's own contract instead: boot as the
# long-running owner, publish readiness and posture, then SIGTERM and fold
# exactly like the mount — exit 0, durable state intact, lock released.
#
# Shared helpers live in tests/alpha-common.sh (sourced here too); the
# mount-specific halves (start_mount, step-grammar) are not sourced.
set -euo pipefail

HOST_LOGS="/tmp/lima/logs/$(date +%Y%m%d-%H%M%S)"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "$SCRIPT_DIR/e2e-guard.sh"
e2e_require_wrapped "${E2E_ENV_FILE:-/tmp/lima/e2e-env.sh}" "./lima/run-alpha.sh" \
  || { echo "  FAIL: unwrapped leg invocation refused" >&2; exit 2; }
# shellcheck source=/dev/null
source "$SCRIPT_DIR/alpha-common.sh"

[[ -n "${WYRD_APP_BIN:-}" && -x "$WYRD_APP_BIN" ]] \
  || die "host leg needs an executable WYRD_APP_BIN from e2e-env.sh"

# --- step 1: a drive the host can own ----------------------------------
host_step1_init() {
  step 1 "drive the host can own"
  local d="$DRIVES/host" c="$CREDS/host"
  mkdir -p "$c"
  gen_identity "$c/identity"; gen_passphrase "$c/passphrase"
  expect_exit 0 host-init with_creds "$c" init "$d" >/dev/null
  pass "init creates the host drive"
}

# --- step 2: boot as the long-running owner ----------------------------
host_step2_boot() {
  step 2 "host boots and publishes readiness"
  local d="$DRIVES/host" c="$CREDS/host"
  # Background the binary directly, never through a helper function:
  # a nested function call defeats bash's exec optimization, leaving
  # $! pointing at an intermediate shell (whose SigCgt, not the
  # host's, the probe below would read — and whose death on SIGTERM
  # would orphan the host instead of folding clean).
  WYRD_PASSPHRASE="$(cat "$c/passphrase")" \
    WYRD_IDENTITY_HEX="$(cat "$c/identity")" \
    "$WYRD_APP_BIN" "$d" --loopback \
    >"$LOGDIR/host.out" 2>"$LOGDIR/host.err" &
  echo $! > "$E2E_ROOT/host.pid"
  poll_until 60 grep -q "host ready: serving " "$LOGDIR/host.err" \
    || die "host never printed its ready line (see host.err)"
  pass "host ready line names the serving id"
  [[ -f "$d/vault.posture" ]] \
    || die "host published no posture sidecar beside the drive"
  pass "posture sidecar published beside the drive"
  # The TERM below must trip the installed handler, not the default
  # disposition: bit 15 (SIGTERM) in the caught-signal mask proves the
  # handler is armed. An immediate 143 later means teardown failed;
  # a missing bit means the handler never installed.
  local caught
  caught="0x$(awk '/^SigCgt:/{print $2}' "/proc/$(cat "$E2E_ROOT/host.pid")/status")"
  (( caught & 0x4000 )) \
    || die "host has no SIGTERM handler armed (SigCgt=$caught)"
  pass "SIGTERM handler armed in the host"
}

# --- step 3: SIGTERM folds like the mount -------------------------------
host_step3_term() {
  step 3 "SIGTERM folds clean"
  local pid
  pid="$(cat "$E2E_ROOT/host.pid")"
  kill -TERM "$pid"
  poll_until 30 bash -c "! kill -0 $pid 2>/dev/null" \
    || { kill -KILL "$pid" 2>/dev/null || true; die "host ignored SIGTERM"; }
  set +e
  wait "$pid"
  local got=$?
  set -e
  [[ $got -eq 0 ]] || die "host exited $got, want 0 (see host.err)"
  pass "host exits 0 on SIGTERM"
}

# --- step 4: the lock is released and state is durable ------------------
host_step4_reopen() {
  step 4 "lock released, state durable"
  local d="$DRIVES/host" c="$CREDS/host"
  expect_exit 0 host-reopen with_creds "$c" device "$d" id >/dev/null
  pass "drive reopens after the host quits"
}

# --- step 5: no secret bytes reach the logs ----------------------------
host_step5_leaks() {
  step 5 "no secret bytes in host logs"
  local c="$CREDS/host"
  check_no_leaks "$LOGDIR/host.out" "$(cat "$c/identity")" "$(cat "$c/passphrase")"
  check_no_leaks "$LOGDIR/host.err" "$(cat "$c/identity")" "$(cat "$c/passphrase")"
  pass "host logs carry no secrets"
}

main() {
  reap_background() {
    e2e_timing_report
    if [[ -f "$E2E_ROOT/host.pid" ]]; then
      local pid
      pid="$(cat "$E2E_ROOT/host.pid" 2>/dev/null || true)"
      if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill -KILL "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
      fi
    fi
    collect_logs
  }
  trap reap_background EXIT
  shopt -s nullglob dotglob
  sudo find "${E2E_ROOT:?}" -mindepth 1 -maxdepth 1 ! -name 'e2e-env.sh' -exec rm -rf {} +
  shopt -u nullglob dotglob
  sudo mkdir -p "$DRIVES" "$CREDS" "$MNTS" "$LOGDIR"
  sudo chmod 0777 "$DRIVES" "$CREDS" "$MNTS" "$LOGDIR"
  host_step1_init
  host_step2_boot
  host_step3_term
  host_step4_reopen
  host_step5_leaks
  e2e_timing_report
  echo "host-leg: $PASS_COUNT checks passed"
}

main "$@"
