#!/usr/bin/env bats
# shellcheck disable=SC2034
# (GATE_GUEST_BIN and friends are read indirectly by on_o/on_n from
# tests/harness/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate/gate-phase-03.bats — gate-path phase 3: cross-host convergence.
#
# Ports legacy microVM phase 7: owner/member legs
# converge across hosts, then the host asserts control-plane opacity
# (kind-1059 gift wraps only, member addressed and never authoring,
# no bare kind-9501 rumors) and runs the wave-2 embedded-consumer
# check. Runs as the build user under tests/gate/gate-entry.sh, after
# phases 1-2 in the same bats invocation: the pairing material this
# phase inspects ($RUN/pairing-n.txt) was ferried host-side by phase 2
# (pulled from the member peer), and is re-derived here from the file
# (bats runs each @test in its own process, so no shell variable
# crosses phases).

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN NAK_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN GATE_PARTITION_BIN CORE_ONLY GUEST_ENV RELAY_URL; do
    [ -n "${!v+x}" ] || { echo "gate-phase-03: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/var/lib/wyrd-e2e/e2e-env.sh" ] || { echo "gate-phase-03: GUEST_ENV=$GUEST_ENV is not the guest env file" >&2; return 1; }
  load "$GATE_BATS_LIB/guest"
  load "$GATE_BATS_LIB/partition"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

@test "owner and member converge across hosts with an opaque control plane" {
  mkdir -p "$RUN/logs"
  guest_exec peer-o -- rm -f "$GUEST_RUN/signals/owner-done" "$GUEST_RUN/signals/owner-w2-written" "$GUEST_RUN"/wave2-owner-list.txt
  guest_exec peer-n -- rm -f "$GUEST_RUN/signals/member-w2-done" "$GUEST_RUN"/wave2-member-list.txt
  OD="$GUEST_RUN/drives/owner"
  OC="$GUEST_RUN/creds/owner"
  GMD="$GUEST_RUN/drives/member-n"
  GMC="$GUEST_RUN/creds/member-n"
  # Legs run backgrounded with output to log files (not `run`: their
  # output is voluminous and the failure die names the log, as in the
  # legacy phase).
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh converge-member $GMD $GMC $RELAY_URL" \
    >"$RUN/logs/leg-member.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh converge-owner $OD $OC $RELAY_URL" \
    >"$RUN/logs/leg-owner.out" 2>&1 &
  LEG_O=$!
  wait "$LEG_N" || { echo "member convergence leg failed (see logs/leg-member.out)"; false; }
  wait "$LEG_O" || { echo "owner convergence leg failed (see logs/leg-owner.out)"; false; }
  # Control-plane opacity: the relay must only ever carry kind-1059
  # gift wraps — the 9501 rumor travels inside the NIP-59 seal+wrap
  # (trust.md T16), never in cleartext. nak runs in-guest (the image
  # carries it beside wyrd); the runner already gated on relay
  # readiness, so an empty answer is a failure, not connection
  # flakiness. Both queries below are sample-relative at limit 500.
  # Stdin filter form (not -k/-l flags): the channel's nak answers
  # flag-built filters with zero events on this relay while the stdin
  # form reads fine — observed on odin, so pin the working form.
  REQ1059='{"kinds":[1059],"limit":500}'
  REQ9501='{"kinds":[9501],"limit":500}'
  WRAPS="$(printf '%s' "$REQ1059" | guest_timeout 30 peer-o -- "$NAK_BIN" req "$RELAY_URL" 2>"$RUN/logs/nak-1059.err" | grep '"kind":1059' || true)"
  [[ -n "$WRAPS" ]] || { echo "no kind-1059 wraps on the relay: control plane never flowed (see logs/nak-1059.err)"; false; }
  [[ -z "$(printf '%s\n' "$WRAPS" | grep -v '"p"' || true)" ]] \
    || { echo "gift wraps without recipient p tags on the relay (see logs/nak-1059.err)"; false; }
  # Identity shape: the member device is addressed, and no wrap is
  # signed by a device key — authors are discarded ephemeral keys
  # (trust.md T16). No census: shared-core step 4 mints throwaway
  # pairing devices that leave their own relay traces, so the p set
  # legitimately holds more than this run's pair. Base64 ciphertext
  # holds no quotes, so '"p","<hex>"' only matches relay JSON
  # structure (tags are arrays, hence the comma).
  DEV="$(grep '^device ' "$RUN/pairing-n.txt" | cut -d' ' -f2)"
  P_HEX="$(printf '%s\n' "$WRAPS" | grep -oE '"p","[0-9a-f]{64}' | grep -oE '[0-9a-f]{64}' | sort -u || true)"
  [[ -n "$P_HEX" ]] || { echo "no recipient p tags on the relay (see logs/nak-1059.err)"; false; }
  printf '%s\n' "$P_HEX" | grep -qxF "$DEV" \
    || { echo "member device $DEV never addressed on the relay (see logs/nak-1059.err)"; false; }
  AUTHORS_HEX="$(printf '%s\n' "$WRAPS" | grep -oE '"pubkey":"[0-9a-f]{64}"' | grep -oE '[0-9a-f]{64}' | sort -u || true)"
  [[ -n "$AUTHORS_HEX" ]] || { echo "no wrap authors on the relay (see logs/nak-1059.err)"; false; }
  # The member device must never author: of the two device keys the
  # host provably holds one ($DEV), so this is the precise half of
  # "no device key signs" — the owner half is not established here
  # (its pubkey never appears as a proven p recipient), and the check
  # stays on the 1059 layer only (the inner kind-13 seal is the
  # sender's identity key by design, trust.md T16).
  printf '%s\n' "$AUTHORS_HEX" | grep -qxF "$DEV" \
    && { echo "member device $DEV authored a gift wrap: authors must be ephemeral (see logs/nak-1059.err)"; false; }
  # Capture-then-assert (never grep -q in the pipeline): grep -q
  # exits at the first match and SIGPIPEs nak, which under pipefail
  # turns a real multi-event leak into a silent pass. Draining to a
  # file keeps the failure reachable exactly when it matters, and the
  # status is captured outside any command substitution — an
  # assignment inside $() dies with the subshell, so a timeout would
  # otherwise read as success (pre-existing shape, fixed here).
  nak_status=0
  { printf '%s' "$REQ9501" | guest_timeout 30 peer-o -- "$NAK_BIN" req "$RELAY_URL" \
    >"$RUN/logs/nak-9501.raw" 2>"$RUN/logs/nak-9501.err" || nak_status=$?; }
  RUMORS="$(grep '"kind":9501' "$RUN/logs/nak-9501.raw" || true)"
  # The leak query's real bound is the timeout above, not limit 500: a
  # zero-result subscription depends on the relay terminating it, so
  # "no rumor in thirty seconds" is what the pass below proves.
  # Both timeout codes fail closed: 124 is the TERM bound, 137 the
  # KILL grace, and either one makes the leak check inconclusive.
  [[ "$nak_status" != 124 && "$nak_status" != 137 ]] \
    || { echo "9501 query timed out (status $nak_status): leak check inconclusive (see logs/nak-9501.err)"; false; }
  [[ -z "$RUMORS" ]] || { echo "relay carries a bare kind-9501 rumor: control leaked in cleartext (see logs/nak-9501.err)"; false; }
  # Wave 2 (sync-convergence issue): the embedded consumer converges
  # without mounting. The owner writes a second wave through FUSE and
  # stays up serving; the member fetches it headless and reads the
  # converged state attachment-free (`snapshot heads`: heads with tree
  # hashes, no timestamps, so the host diff is exact). Identical
  # live heads mean identical manifests, hence identical bytes by
  # CAS. Placed after the opacity checks so wave-2 control traffic
  # cannot push the relay sample past the queries' limit headroom.
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh converge2-member $GMD $GMC $RELAY_URL" \
    >"$RUN/logs/leg-member-w2.out" 2>&1 &
  LEG_N=$!
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh converge2-owner $OD $OC $RELAY_URL" \
    >"$RUN/logs/leg-owner-w2.out" 2>&1 &
  LEG_O=$!
  # Both statuses captured (review F3): the member-first wait would
  # otherwise report the member's exhausted retry loop while the
  # owner's real failure sits unmentioned in its own log. Name both
  # logs in the one failure that fires.
  n_rc=0; o_rc=0
  wait "$LEG_N" || n_rc=$?
  wait "$LEG_O" || o_rc=$?
  [[ "$n_rc" == 0 ]] || { echo "member wave-2 leg failed (see logs/leg-member-w2.out; owner status $o_rc, see logs/leg-owner-w2.out)"; false; }
  [[ "$o_rc" == 0 ]] || { echo "owner wave-2 leg failed (see logs/leg-owner-w2.out)"; false; }
  guest_pull peer-o "$GUEST_RUN/wave2-owner-list.txt" "$RUN/wave2-owner-list.txt" \
    || { echo "owner list pull failed"; false; }
  guest_pull peer-n "$GUEST_RUN/wave2-member-list.txt" "$RUN/wave2-member-list.txt" \
    || { echo "member list pull failed"; false; }
  diff "$RUN/wave2-owner-list.txt" "$RUN/wave2-member-list.txt" \
    || { echo "embedded and FUSE views diverge on live heads (see wave2-*-list.txt)"; false; }
}
