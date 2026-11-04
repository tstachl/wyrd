#!/usr/bin/env bats
# shellcheck disable=SC2034
# (GATE_GUEST_BIN and friends are read indirectly by on_o/on_n from
# tests/harness/e2e-helpers.bash through `run`, which shellcheck cannot see
# through; every assignment below is consumed at call time.)
# tests/gate/gate-phase-08.bats — gate-path phase 8: headless serving.
#
# Ports legacy microVM phase 12: two headless
# peers exchange content with neither mounting — the owner drive
# serves via `wyrd vault` while fresh devices join and converge
# with plain `sync now`, reading bytes through an attachment (the
# read faults chunks in over the serve route). The dataset is a
# fresh single-author drive; C proves the positive case, a KILL
# mid-residency plus restart proves the mirror rebuilds, E fetches
# against the immediate KILL replacement, D is admitted after the
# restart so its obligations are fresh for serve#3. Runs as the
# build user under tests/gate/gate-entry.sh, after phases 1-7 in the
# same bats invocation. All drive/cred paths are recomputed here
# (bats runs each @test in its own process, so no shell variable
# crosses phases).

bats_require_minimum_version 1.5.0

load "$BATS_TEST_DIRNAME/../harness/e2e-guard.sh"

setup() {
  for v in STATE_DIR WYRD_BIN WYRD_E2E_TOKEN WYRD_E2E_LOCK GATE_BATS_LIB GATE_GUEST_BIN GATE_PARTITION_BIN CORE_ONLY GUEST_ENV RELAY_URL; do
    [ -n "${!v+x}" ] || { echo "gate-phase-08: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/var/lib/wyrd-e2e/e2e-env.sh" ] || { echo "gate-phase-08: GUEST_ENV=$GUEST_ENV is not the guest env file" >&2; return 1; }
  load "$GATE_BATS_LIB/guest"
  load "$GATE_BATS_LIB/partition"
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
}

# mint_pair <role> <creds-dir> <drive-dir>: create credential material
# in-guest with the suite's standard sizes, owned by the guest that
# will use it. Secrets never touch the host.
mint_pair() {
  mint_guest_creds "$@"
}

@test "three headless peers exchange content with neither mounting" {
  mkdir -p "$RUN/logs"
  guest_exec peer-o rm -f "$GUEST_RUN"/signals/headless-setup-done "$GUEST_RUN"/signals/headless-serve-ready \
    "$GUEST_RUN"/signals/headless-serve2-ready "$GUEST_RUN"/signals/headless-serve-stopped \
    "$GUEST_RUN"/signals/headless-d-invited "$GUEST_RUN"/signals/headless-serve3-ready
  guest_exec peer-n rm -f "$GUEST_RUN"/signals/headless-fetch-c-done "$GUEST_RUN"/signals/headless-fetch-e-done \
    "$GUEST_RUN"/signals/headless-fetch-d-done
  GHO="$GUEST_RUN/creds/headless-owner"
  GHF="$GUEST_RUN/drives/headless-fresh"
  mint_pair peer-o "$GHO" "$GHF"
  # Post-mortem parity with the legacy phase: persist each wyrd
  # invocation's captured output to $RUN/logs before asserting (bats
  # merges stderr into $output, so one .out per step).
  run guest_wyrd peer-o init --identity-file "$GHO/identity" --passphrase-file "$GHO/passphrase" \
    "$GHF"
  printf '%s\n' "$output" >"$RUN/logs/init-hf.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh headless-setup $GHF $GHO" \
    >"$RUN/logs/leg-headless-setup.out" 2>&1 &
  LEG_HSETUP=$!
  wait_guest_file peer-o 300 "$GUEST_RUN/signals/headless-setup-done" \
    "setup leg never wrote the dataset (see logs/leg-headless-setup.out)" "$LEG_HSETUP" \
    || return 1
  wait "$LEG_HSETUP" || die "headless setup leg failed (see logs/leg-headless-setup.out)"
  GHC="$GUEST_RUN/creds/headless-c"
  GHD="$GUEST_RUN/drives/headless-c"
  mint_pair peer-n "$GHC" "$GHD"
  run guest_wyrd peer-n device --identity-file "$GHC/identity" --passphrase-file "$GHC/passphrase" \
    "$GHD" pairing-request "$GUEST_RUN/pairing-hc.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-hc.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-n "$GUEST_RUN/pairing-hc.txt" "$RUN/pairing-hc.txt" \
    || { echo "pairing-hc pull failed"; false; }
  DEV_C="$(grep '^device ' "$RUN/pairing-hc.txt" | cut -d' ' -f2)"
  KEY_C="$(grep '^encryption-key ' "$RUN/pairing-hc.txt" | cut -d' ' -f2)"
  [[ "${#DEV_C}" == 64 && "${#KEY_C}" == 64 ]] || die "headless-c pairing material malformed"
  run guest_wyrd peer-o member --identity-file "$GHO/identity" \
    --passphrase-file "$GHO/passphrase" "$GHF" \
    invite "$DEV_C" "$KEY_C" "$GUEST_RUN/invitation-hc"
  printf '%s\n' "$output" >"$RUN/logs/invite-hc.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-o "$GUEST_RUN/invitation-hc" "$RUN/invitation-hc" \
    || { echo "invitation-hc pull failed"; false; }
  guest_push_e2e peer-n "$RUN/invitation-hc" "$GUEST_RUN/invitation-hc" \
    || { echo "invitation-hc push failed"; false; }
  run guest_wyrd peer-n device --identity-file "$GHC/identity" --passphrase-file "$GHC/passphrase" \
    "$GHD" join "$GUEST_RUN/invitation-hc"
  printf '%s\n' "$output" >"$RUN/logs/join-hc.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  GHE="$GUEST_RUN/creds/headless-e"
  GHED="$GUEST_RUN/drives/headless-e"
  mint_pair peer-n "$GHE" "$GHED"
  run guest_wyrd peer-n device --identity-file "$GHE/identity" --passphrase-file "$GHE/passphrase" \
    "$GHED" pairing-request "$GUEST_RUN/pairing-he.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-he.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-n "$GUEST_RUN/pairing-he.txt" "$RUN/pairing-he.txt" \
    || { echo "pairing-he pull failed"; false; }
  DEV_E="$(grep '^device ' "$RUN/pairing-he.txt" | cut -d' ' -f2)"
  KEY_E="$(grep '^encryption-key ' "$RUN/pairing-he.txt" | cut -d' ' -f2)"
  [[ "${#DEV_E}" == 64 && "${#KEY_E}" == 64 ]] || die "headless-e pairing material malformed"
  run guest_wyrd peer-o member --identity-file "$GHO/identity" \
    --passphrase-file "$GHO/passphrase" "$GHF" \
    invite "$DEV_E" "$KEY_E" "$GUEST_RUN/invitation-he"
  printf '%s\n' "$output" >"$RUN/logs/invite-he.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-o "$GUEST_RUN/invitation-he" "$RUN/invitation-he" \
    || { echo "invitation-he pull failed"; false; }
  guest_push_e2e peer-n "$RUN/invitation-he" "$GUEST_RUN/invitation-he" \
    || { echo "invitation-he push failed"; false; }
  run guest_wyrd peer-n device --identity-file "$GHE/identity" --passphrase-file "$GHE/passphrase" \
    "$GHED" join "$GUEST_RUN/invitation-he"
  printf '%s\n' "$output" >"$RUN/logs/join-he.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh headless-serve $GHF $GHO $RELAY_URL" \
    >"$RUN/logs/leg-headless-serve.out" 2>&1 &
  LEG_HS=$!
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh headless-fetch $GHD $GHC $RELAY_URL c headless-serve-ready headless-fetch-c-done" \
    >"$RUN/logs/leg-headless-fetch-c.out" 2>&1 &
  LEG_HC=$!
  # E waits on serve#2's ready file: it fetches against the immediate
  # KILL replacement, proving the 22b crash-restart gate.
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh headless-fetch $GHED $GHE $RELAY_URL e headless-serve2-ready headless-fetch-e-done" \
    >"$RUN/logs/leg-headless-fetch-e.out" 2>&1 &
  LEG_HE=$!
  # D is admitted mid-leg, after serve#2 stops and frees the owner
  # drive: its obligations must be fresh for serve#3's route. The
  # invite runs in-guest on the owner peer (offline keystore op); the
  # leg waits on headless-d-invited before starting serve#3.
  wait_guest_file peer-o 1200 "$GUEST_RUN/signals/headless-serve-stopped" \
    "serve leg never stopped serve#2 (see logs/leg-headless-serve.out)" "$LEG_HS" \
    || return 1
  GHDD="$GUEST_RUN/creds/headless-d"
  GHDDD="$GUEST_RUN/drives/headless-d"
  mint_pair peer-n "$GHDD" "$GHDDD"
  run guest_wyrd peer-n device --identity-file "$GHDD/identity" --passphrase-file "$GHDD/passphrase" \
    "$GHDDD" pairing-request "$GUEST_RUN/pairing-hd.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-hd.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-n "$GUEST_RUN/pairing-hd.txt" "$RUN/pairing-hd.txt" \
    || { echo "pairing-hd pull failed"; false; }
  DEV_D="$(grep '^device ' "$RUN/pairing-hd.txt" | cut -d' ' -f2)"
  KEY_D="$(grep '^encryption-key ' "$RUN/pairing-hd.txt" | cut -d' ' -f2)"
  [[ "${#DEV_D}" == 64 && "${#KEY_D}" == 64 ]] || die "headless-d pairing material malformed"
  run guest_wyrd peer-o member --identity-file "$GHO/identity" \
    --passphrase-file "$GHO/passphrase" "$GHF" \
    invite "$DEV_D" "$KEY_D" "$GUEST_RUN/invitation-hd"
  printf '%s\n' "$output" >"$RUN/logs/invite-hd.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-o "$GUEST_RUN/invitation-hd" "$RUN/invitation-hd" \
    || { echo "invitation-hd pull failed"; false; }
  guest_push_e2e peer-n "$RUN/invitation-hd" "$GUEST_RUN/invitation-hd" \
    || { echo "invitation-hd push failed"; false; }
  run guest_wyrd peer-n device --identity-file "$GHDD/identity" --passphrase-file "$GHDD/passphrase" \
    "$GHDDD" join "$GUEST_RUN/invitation-hd"
  printf '%s\n' "$output" >"$RUN/logs/join-hd.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_exec peer-o touch "$GUEST_RUN/signals/headless-d-invited"
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh headless-fetch $GHDDD $GHDD $RELAY_URL d headless-serve3-ready headless-fetch-d-done" \
    >"$RUN/logs/leg-headless-fetch-d.out" 2>&1 &
  LEG_HD=$!
  wait "$LEG_HC" || die "headless fetch-c leg failed (see logs/leg-headless-fetch-c.out)"
  wait "$LEG_HE" || die "headless fetch-e leg failed (see logs/leg-headless-fetch-e.out)"
  wait "$LEG_HD" || die "headless fetch-d leg failed (see logs/leg-headless-fetch-d.out)"
  wait "$LEG_HS" || die "headless serve leg failed (see logs/leg-headless-serve.out)"
}

@test "pause defers the outbox across an ungraceful restart" {
  mkdir -p "$RUN/logs"
  GPO="$GUEST_RUN/creds/pause-owner"
  GPD="$GUEST_RUN/drives/pause"
  mint_pair peer-o "$GPO" "$GPD"
  run guest_wyrd peer-o init --identity-file "$GPO/identity" --passphrase-file "$GPO/passphrase" \
    "$GPD"
  printf '%s\n' "$output" >"$RUN/logs/init-pause.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  GPM="$GUEST_RUN/creds/pause-member"
  GPMD="$GUEST_RUN/drives/pause-member"
  mint_pair peer-n "$GPM" "$GPMD"
  run guest_wyrd peer-n device --identity-file "$GPM/identity" --passphrase-file "$GPM/passphrase" \
    "$GPMD" pairing-request "$GUEST_RUN/pairing-pm.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-pm.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-n "$GUEST_RUN/pairing-pm.txt" "$RUN/pairing-pm.txt" \
    || { echo "pairing-pm pull failed"; false; }
  DEV_M="$(grep '^device ' "$RUN/pairing-pm.txt" | cut -d' ' -f2)"
  KEY_M="$(grep '^encryption-key ' "$RUN/pairing-pm.txt" | cut -d' ' -f2)"
  [[ "${#DEV_M}" == 64 && "${#KEY_M}" == 64 ]] || die "pause-member pairing material malformed"
  run guest_wyrd peer-o member --identity-file "$GPO/identity" \
    --passphrase-file "$GPO/passphrase" "$GPD" \
    invite "$DEV_M" "$KEY_M" "$GUEST_RUN/invitation-pm"
  printf '%s\n' "$output" >"$RUN/logs/invite-pm.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  guest_pull peer-o "$GUEST_RUN/invitation-pm" "$RUN/invitation-pm" \
    || { echo "invitation-pm push failed"; false; }
  guest_push_e2e peer-n "$RUN/invitation-pm" "$GUEST_RUN/invitation-pm" \
    || { echo "invitation-pm push failed"; false; }
  run guest_wyrd peer-n device --identity-file "$GPM/identity" --passphrase-file "$GPM/passphrase" \
    "$GPMD" join "$GUEST_RUN/invitation-pm"
  printf '%s\n' "$output" >"$RUN/logs/join-pm.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh pause-owner $GPD $GPO $RELAY_URL" \
    >"$RUN/logs/leg-pause-owner.out" 2>&1 &
  LEG_PO=$!
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh pause-member $GPMD $GPM $RELAY_URL" \
    >"$RUN/logs/leg-pause-member.out" 2>&1 &
  LEG_PM=$!
  wait "$LEG_PO" || die "pause owner leg failed (see logs/leg-pause-owner.out)"
  wait "$LEG_PM" || die "pause member leg failed (see logs/leg-pause-member.out)"
}
