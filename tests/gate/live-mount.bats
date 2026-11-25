#!/usr/bin/env bats
# shellcheck disable=SC2034
# (GATE_GUEST_BIN and friends are read indirectly by on_o/guest_timeout
# through `run`, which shellcheck cannot see through.)
# tests/gate/live-mount.bats — the ignored live-mount libtests on a
# Linux FUSE guest. Runs as the build user under
# tests/gate/live-mount-entry.sh; Odin-only (agent transport, no ssh).
#
# The two tests are #[ignore]d because they need kernel FUSE and local
# networking. They are library tests: the harness is the lib target's
# libtest binary baked into the peer-o image, not `wyrd --bin`. They
# must run in one process (the shutdown latch is process-global and the
# serial mutex only serializes inside a process), so this leg invokes
# the binary once with a substring filter for both names. It asserts
# that two tests actually ran: a zero-test green would be vacuous.

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN \
    GUEST_ENV GUEST_RUN LIVE_MOUNT_TEST_BIN PEER_O_IP; do
    [ -n "${!v+x}" ] || { echo "live-mount: $v is not set (run through tests/gate/live-mount-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/var/lib/wyrd-e2e-live-mount/e2e-env.sh" ] \
    || { echo "live-mount: GUEST_ENV=$GUEST_ENV is not the guest env file" >&2; return 1; }
  load "$GATE_BATS_LIB/guest"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/live-mount-entry.sh (gate task)" --check-owner-pid
}

@test "ignored live-mount libtests pass on a Linux FUSE guest" {
  # Distinguish an environment failure from a product failure before
  # running anything: the peer images rely on the default kernel FUSE,
  # while the drive-service guest loads the module explicitly.
  run on_o "test -r /dev/fuse && command -v fusermount3 && test -x '$LIVE_MOUNT_TEST_BIN'"
  [ "$status" -eq 0 ] || { echo "$output"; false; }

  # Both tests also need local networking for their serving endpoint.
  # A broken loopback must be named as an environment failure, never
  # reported as a product regression. Bind and connect on 127.0.0.1 in
  # the guest itself: this proves the address and TCP loopback work
  # without depending on any external listener.
  run on_o "ip -o addr show lo | grep >/dev/null '127.0.0.1' && python3 -c 'import socket; s=socket.socket(); s.bind((\"127.0.0.1\",0)); s.listen(1); p=s.getsockname()[1]; c=socket.create_connection((\"127.0.0.1\",p),1); c.close(); s.close()'"
  [ "$status" -eq 0 ] || {
    echo "live-mount: guest loopback networking is unavailable (environment, not product)"
    echo "$output"
    false
  }

  # Hard-bound the transport call itself (timeout cannot wrap a shell
  # function). The libtest runs both ignored tests in one process;
  # redirect its output into the guest log tree so the task's
  # guest_log_dir fetch carries the evidence.
  run guest_timeout 300 peer-o runuser -u e2e -g users -G wheel -- \
    /usr/bin/env PATH=/run/wrappers/bin:/run/current-system/sw/bin:/usr/bin:/bin \
    bash -c "set -o pipefail; '$LIVE_MOUNT_TEST_BIN' --ignored live_mount_ >'$GUEST_RUN/logs/live-mount.out' 2>&1"
  if [ "$status" -ne 0 ]; then
    echo "live-mount: libtest failed with status $status; guest log:"
    run on_o "cat '$GUEST_RUN/logs/live-mount.out'"
    echo "$output"
    false
  fi

  run on_o "cat '$GUEST_RUN/logs/live-mount.out'"
  echo "$output" | grep >/dev/null 'running 2 tests' \
    || { echo "live-mount: both ignored tests did not run"; echo "$output"; false; }
  echo "$output" | grep >/dev/null 'test result: ok. 2 passed' \
    || { echo "live-mount: libtest did not report two passes"; echo "$output"; false; }
}
