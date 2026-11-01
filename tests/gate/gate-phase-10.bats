#!/usr/bin/env bats
# shellcheck disable=SC2034
# (GATE_GUEST_BIN and friends are read indirectly by on_o/on_n from
# tests/harness/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate/gate-phase-10.bats — gate-path phase 10: resource exhaustion.
#
# Ports the legacy exhaustion leg: fill the
# exhaustion drive's filesystem to 256 MiB headroom, then write
# through an offline attachment until ENOSPC. Offline on purpose:
# exhaustion is a storage-stack property, not a sync one, and a
# relay would only add failure modes to the wrong leg. The drive
# lives on a fixed-size ext4 image loop-attached IN the guest
# (never on shared state): the fill is guest-disk-local. init and
# export run in-guest (the container is invisible host-side); the
# harness pulls the exported tree and asserts the fail-closed half
# on it. Runs as the build user under tests/gate/gate-entry.sh,
# after phases 1-9 in the same bats invocation. All drive/cred
# paths are recomputed here (bats runs each @test in its own
# process, so no shell variable crosses phases).

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN GATE_PARTITION_BIN CORE_ONLY GUEST_ENV RELAY_URL; do
    [ -n "${!v+x}" ] || { echo "gate-phase-10: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/var/lib/wyrd-e2e/e2e-env.sh" ] || { echo "gate-phase-10: GUEST_ENV=$GUEST_ENV is not the guest env file" >&2; return 1; }
  load "$GATE_BATS_LIB/guest"
  load "$GATE_BATS_LIB/partition"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

@test "writes fail closed ENOSPC and the drive recovers" {
  mkdir -p "$RUN/logs"
  guest_exec peer-o rm -f "$GUEST_RUN"/fill-*.stderr "$GUEST_RUN"/exh-failed-fill
  GEC="$GUEST_RUN/creds/exh"
  GED="$GUEST_RUN/drives/exh"
  # Guest disk guard: the contained image costs ~2.3 GiB. Fail loudly
  # here instead of dying silently mid-fill when the guest is already
  # sick.
  avail_kb="$(guest_exec peer-o df -k --output=avail / | tail -n 1 | tr -d ' ')"
  [[ "$avail_kb" -ge 4194304 ]] \
    || die "guest disk has only ${avail_kb} KiB free: phase 10 needs 4 GiB for the contained exhaustion image"
  GIMG="$GUEST_RUN/exh-fs.img"
  guest_exec peer-o rm -f "$GIMG"
  guest_exec peer-o fallocate -l 2415919104 "$GIMG" \
    || die "exhaustion image preallocation failed"
  mint_guest_creds peer-o "$GEC" "$GED"
  # Filesystem setup, init, and export run in-guest (escalation
  # there is guest privilege, not host privilege): the container is
  # invisible host-side. Post-mortem parity: persist captured output
  # to $RUN/logs before asserting (bats merges stderr into $output).
  run on_o "sudo mkfs.ext4 -q -m 0 '$GIMG'"
  printf '%s\n' "$output" >"$RUN/logs/mkfs-exh.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run on_o "sudo mount -o loop '$GIMG' '$GED' && sudo chown e2e:users '$GED'"
  printf '%s\n' "$output" >"$RUN/logs/attach-exh-fs.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run on_o "$WYRD_BIN init --identity-file '$GEC/identity' --passphrase-file '$GEC/passphrase' '$GED'"
  printf '%s\n' "$output" >"$RUN/logs/init-exh.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh exhaustion $GED $GEC"
  printf '%s\n' "$output" >"$RUN/logs/leg-exhaustion.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_exec peer-o rm -rf "$GUEST_RUN/export-exh"
  run on_o "$WYRD_BIN export --identity-file '$GEC/identity' --passphrase-file '$GEC/passphrase' '$GED' '$GUEST_RUN/export-exh'"
  printf '%s\n' "$output" >"$RUN/logs/export-exh.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  rm -rf "$RUN/export-exh"
  guest_pull peer-o "$GUEST_RUN/export-exh" "$RUN/export-exh" \
    || { echo "exhaustion export pull failed"; false; }
  [[ "$(cat "$RUN/export-exh/small.txt")" == "pre-fill" ]] \
    || die "exhaustion export lost the pre-fill anchor"
  [[ "$(cat "$RUN/export-exh/recovery.txt")" == "recovered" ]] \
    || die "exhaustion export lost the recovery anchor"
  # Fill-file inventory against the write-path commit contract
  # (docs/contracts/write-path.md: `create` commits the empty file as its own
  # snapshot; a refused write commits nothing of the write; a ceiling
  # refuses at the commit boundary, never partially). The leg records
  # the first failing fill index; each file is judged by what its own
  # operation reported —
  #   before the failing index: writes reported success, so content
  #     must exist byte-complete (absent means committed bytes lost
  #     across reopen; partial means torn state);
  #   at the failing index: the create half may have committed (empty
  #     file, write refused) or been refused outright (absent) — both
  #     are contract-legal; anything else contradicts the op report;
  #   after the failing index: never attempted, so must be absent
  #     (present means phantom state).
  # Present-but-partial anywhere is the torn partial-accept this
  # phase exists to forbid. The index file lives beside the fill
  # stderrs in the guest state root — never on the exhausted fs, so
  # the failure handler does not depend on the filesystem under test.
  guest_exec peer-o test -f "$GUEST_RUN/exh-failed-fill" \
    || die "exhaustion leg never recorded the failing fill index"
  _fidx="$(guest_exec peer-o cat "$GUEST_RUN/exh-failed-fill")"
  [[ "$_fidx" =~ ^[0-9]+$ ]] && (( _fidx >= 1 && _fidx <= 24 )) \
    || die "exhaustion leg recorded a bogus failing fill index: $_fidx"
  for (( _x = 1; _x <= 24; _x++ )); do
    _f="$RUN/export-exh/fill-$_x.bin"
    if (( _x < _fidx )); then
      [[ -e "$_f" ]] \
        || die "exhaustion export lost fill-$_x.bin: its write reported success"
      _sz="$(stat -c %s "$_f")"
      [[ "$_sz" == "33554432" ]] \
        || die "exhaustion export holds torn fill-$_x.bin ($_sz bytes)"
    elif (( _x == _fidx )); then
      if [[ ! -e "$_f" ]]; then
        : # failing fill absent: create refused with the write — contract-legal
      else
        _sz="$(stat -c %s "$_f")"
        [[ "$_sz" == "0" ]] \
          || die "exhaustion export holds fill-$_x.bin ($_sz bytes) after its write reported failure"
      fi
    else
      [[ ! -e "$_f" ]] \
        || die "exhaustion export holds unattempted fill-$_x.bin: phantom file"
    fi
  done
  unset _x _f _sz _fidx
  # Best-effort detach: the loop device dies with the VM anyway;
  # this just keeps reruns tidy.
  on_o "sudo umount '$GED'" >"$RUN/logs/detach-exh-fs.out" 2>&1 || true
  guest_exec peer-o rm -f "$GIMG" || true
}
