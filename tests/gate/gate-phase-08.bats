#!/usr/bin/env bats
# shellcheck disable=SC2034
# (SSH/SSH_OPTS are read indirectly by on_o/on_n from
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
  for v in STATE_DIR WYRD_BIN PEER_O PEER_N RELAY_URL WYRD_E2E_TOKEN WYRD_E2E_LOCK SSH_KEY GATE_BATS_LIB GATE_PARTITION_BIN CORE_ONLY GUEST_ENV; do
    [ -n "${!v+x}" ] || { echo "gate-phase-08: $v is not set (run through tests/gate/gate-entry.sh)" >&2; return 1; }
  done
  RUN="$STATE_DIR/run"
  [ "$GUEST_ENV" = "/mnt/wyrd-state/run/e2e-env.sh" ] || { echo "gate-phase-08: GUEST_ENV=$GUEST_ENV is not the guest-visible env file" >&2; return 1; }
  source "$BATS_TEST_DIRNAME/../harness/e2e-helpers.bash"
  e2e_require_wrapped "$RUN/e2e-env.sh" "./tests/gate/gate-entry.sh (gate task)" --check-owner-pid
  SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o BatchMode=yes"
  SSH="ssh $SSH_OPTS"
}

# mint_pair <creds-dir> <drives-dir>: create credential material
# with the suite's standard sizes and hand it to the guest owner.
mint_pair() {
  local c="$1" d="$2"
  mkdir -p "$c" "$d"
  head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$c/identity"
  printf 'e2e-%s\n' "$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')" > "$c/passphrase"
  chmod 600 "$c/identity" "$c/passphrase"
  hand_to_guest "$c" "$d"
}

@test "three headless peers exchange content with neither mounting" {
  mkdir -p "$RUN/logs"
  rm -f "$RUN/headless-setup-done" "$RUN/headless-serve-ready" "$RUN/headless-fetch-c-done" \
    "$RUN/headless-serve2-ready" "$RUN/headless-fetch-e-done" \
    "$RUN/headless-serve-stopped" "$RUN/headless-d-invited" \
    "$RUN/headless-serve3-ready" "$RUN/headless-fetch-d-done"
  HO="$RUN/creds/headless-owner"
  HF="$RUN/drives/headless-fresh"
  GHO="$GUEST_RUN/creds/headless-owner"
  GHF="$GUEST_RUN/drives/headless-fresh"
  mint_pair "$HO" "$HF"
  # Post-mortem parity with the legacy phase: persist each wyrd
  # invocation's captured output to $RUN/logs before asserting (bats
  # merges stderr into $output, so one .out per step).
  run as_guest "$WYRD_BIN" init --identity-file "$HO/identity" --passphrase-file "$HO/passphrase" \
    "$HF"
  printf '%s\n' "$output" >"$RUN/logs/init-hf.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  on_o "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh headless-setup $GHF $GHO" \
    >"$RUN/logs/leg-headless-setup.out" 2>&1 &
  LEG_HSETUP=$!
  for ((i = 0; i < 300 * 2; i++)); do
    [[ -f "$RUN/headless-setup-done" ]] && break
    kill -0 "$LEG_HSETUP" 2>/dev/null || die "setup leg exited before writing the dataset (see logs/leg-headless-setup.out)"
    sleep 0.5
  done
  [[ -f "$RUN/headless-setup-done" ]] || die "setup leg never wrote the dataset"
  wait "$LEG_HSETUP" || die "headless setup leg failed (see logs/leg-headless-setup.out)"
  HC="$RUN/creds/headless-c"
  HD="$RUN/drives/headless-c"
  GHC="$GUEST_RUN/creds/headless-c"
  GHD="$GUEST_RUN/drives/headless-c"
  mint_pair "$HC" "$HD"
  run as_guest "$WYRD_BIN" device --identity-file "$HC/identity" --passphrase-file "$HC/passphrase" \
    "$HD" pairing-request "$RUN/pairing-hc.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-hc.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  DEV_C="$(grep '^device ' "$RUN/pairing-hc.txt" | cut -d' ' -f2)"
  KEY_C="$(grep '^encryption-key ' "$RUN/pairing-hc.txt" | cut -d' ' -f2)"
  [[ "${#DEV_C}" == 64 && "${#KEY_C}" == 64 ]] || die "headless-c pairing material malformed"
  run as_guest "$WYRD_BIN" member --identity-file "$HO/identity" \
    --passphrase-file "$HO/passphrase" "$HF" \
    invite "$DEV_C" "$KEY_C" "$RUN/invitation-hc"
  printf '%s\n' "$output" >"$RUN/logs/invite-hc.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run as_guest "$WYRD_BIN" device --identity-file "$HC/identity" --passphrase-file "$HC/passphrase" \
    "$HD" join "$RUN/invitation-hc"
  printf '%s\n' "$output" >"$RUN/logs/join-hc.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  HE="$RUN/creds/headless-e"
  HED="$RUN/drives/headless-e"
  GHE="$GUEST_RUN/creds/headless-e"
  GHED="$GUEST_RUN/drives/headless-e"
  mint_pair "$HE" "$HED"
  run as_guest "$WYRD_BIN" device --identity-file "$HE/identity" --passphrase-file "$HE/passphrase" \
    "$HED" pairing-request "$RUN/pairing-he.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-he.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  DEV_E="$(grep '^device ' "$RUN/pairing-he.txt" | cut -d' ' -f2)"
  KEY_E="$(grep '^encryption-key ' "$RUN/pairing-he.txt" | cut -d' ' -f2)"
  [[ "${#DEV_E}" == 64 && "${#KEY_E}" == 64 ]] || die "headless-e pairing material malformed"
  run as_guest "$WYRD_BIN" member --identity-file "$HO/identity" \
    --passphrase-file "$HO/passphrase" "$HF" \
    invite "$DEV_E" "$KEY_E" "$RUN/invitation-he"
  printf '%s\n' "$output" >"$RUN/logs/invite-he.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run as_guest "$WYRD_BIN" device --identity-file "$HE/identity" --passphrase-file "$HE/passphrase" \
    "$HED" join "$RUN/invitation-he"
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
  # invite runs on the host (offline keystore op); the leg waits on
  # headless-d-invited before starting serve#3.
  for ((i = 0; i < 600 * 2; i++)); do
    [[ -f "$RUN/headless-serve-stopped" ]] && break
    kill -0 "$LEG_HS" 2>/dev/null || die "serve leg exited before stopping serve#2 (see logs/leg-headless-serve.out)"
    sleep 0.5
  done
  [[ -f "$RUN/headless-serve-stopped" ]] || die "serve leg never stopped serve#2"
  HDD="$RUN/creds/headless-d"
  HDDD="$RUN/drives/headless-d"
  GHDD="$GUEST_RUN/creds/headless-d"
  GHDDD="$GUEST_RUN/drives/headless-d"
  mint_pair "$HDD" "$HDDD"
  run as_guest "$WYRD_BIN" device --identity-file "$HDD/identity" --passphrase-file "$HDD/passphrase" \
    "$HDDD" pairing-request "$RUN/pairing-hd.txt"
  printf '%s\n' "$output" >"$RUN/logs/pairing-hd.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  DEV_D="$(grep '^device ' "$RUN/pairing-hd.txt" | cut -d' ' -f2)"
  KEY_D="$(grep '^encryption-key ' "$RUN/pairing-hd.txt" | cut -d' ' -f2)"
  [[ "${#DEV_D}" == 64 && "${#KEY_D}" == 64 ]] || die "headless-d pairing material malformed"
  run as_guest "$WYRD_BIN" member --identity-file "$HO/identity" \
    --passphrase-file "$HO/passphrase" "$HF" \
    invite "$DEV_D" "$KEY_D" "$RUN/invitation-hd"
  printf '%s\n' "$output" >"$RUN/logs/invite-hd.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  run as_guest "$WYRD_BIN" device --identity-file "$HDD/identity" --passphrase-file "$HDD/passphrase" \
    "$HDDD" join "$RUN/invitation-hd"
  printf '%s\n' "$output" >"$RUN/logs/join-hd.out"
  [ "$status" -eq 0 ] || { echo "$output"; false; }
  touch "$RUN/headless-d-invited"
  on_n "E2E_ENV_FILE=$GUEST_ENV bash $GUEST_TESTS/alpha-microvm-legs.sh headless-fetch $GHDDD $GHDD $RELAY_URL d headless-serve3-ready headless-fetch-d-done" \
    >"$RUN/logs/leg-headless-fetch-d.out" 2>&1 &
  LEG_HD=$!
  wait "$LEG_HC" || die "headless fetch-c leg failed (see logs/leg-headless-fetch-c.out)"
  wait "$LEG_HE" || die "headless fetch-e leg failed (see logs/leg-headless-fetch-e.out)"
  wait "$LEG_HD" || die "headless fetch-d leg failed (see logs/leg-headless-fetch-d.out)"
  wait "$LEG_HS" || die "headless serve leg failed (see logs/leg-headless-serve.out)"
}
