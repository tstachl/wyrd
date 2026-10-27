#!/usr/bin/env bash
# tests/test-gate-entry.sh — the gate-path entry assembles the suite
# environment from host-published inputs and refuses bad ones.
#
# Pure bash + stubbed host commands (bats, flock, ssh, timeout): no
# root, no nix, no guest. The stubs record their argv and environment
# instead of acting, so every assertion below pins the driver's
# contract without touching a VM. Run it anywhere:
#   bash tests/test-gate-entry.sh
#
# The smoke suite itself (tests/gate-entry-smoke.bats) is Odin-only:
# it dials real peers. This file pins everything up to exec bats.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENTRY="$ROOT/tests/gate-entry.sh"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

FIXTURE="$(mktemp -d)"
trap 'rm -rf "$FIXTURE"' EXIT
PROVISION_DIR="$FIXTURE/provision"
STATE_DIR="$FIXTURE/state"
RUN_DIR="$STATE_DIR/run"
STUBBIN="$FIXTURE/stubbin"
NOBATS="$FIXTURE/nobats"
CALLS="$FIXTURE/calls"
mkdir -p "$PROVISION_DIR" "$RUN_DIR" "$STUBBIN" "$NOBATS"

printf 'br-wyrd' >"$PROVISION_DIR/bridge"
printf '10.0.7' >"$PROVISION_DIR/subnet"
printf '18761' >"$PROVISION_DIR/relay-port"
printf 'e2e' >"$PROVISION_DIR/ssh-user"
printf '1000' >"$PROVISION_DIR/guest-uid"
printf 'wyrd-.*-virtiofs-wyrd-state\\.sock' >"$PROVISION_DIR/socket-pattern"
printf '%s' "$STATE_DIR" >"$PROVISION_DIR/state-dir"
printf '%s' "$STATE_DIR/lock" >"$PROVISION_DIR/lock-file"
printf 'relay' >"$PROVISION_DIR/relay-role"
printf 'peer-o tap-o 11 ssh runner-peer-o|peer-n tap-n 12 ssh runner-peer-n|relay tap-r 10 - runner-relay' >"$PROVISION_DIR/roles"
printf 'nak packages.nak|runner-peer-o runners.peer-o|runner-peer-n runners.peer-n' >"$PROVISION_DIR/builds"
printf 'key' >"$STATE_DIR/sshkey"
mkdir -p "$FIXTURE/bats-lib"

RUNNER_BIN="$FIXTURE/wyrd-runner"
NAK_BIN="$FIXTURE/nak-runner"
printf '#!/bin/sh\nexit 0\n' >"$RUNNER_BIN"
printf '#!/bin/sh\nexit 0\n' >"$NAK_BIN"
chmod +x "$RUNNER_BIN" "$NAK_BIN"
printf 'wyrd=%s\nnak=%s\nlegacy=ignored\n' "$RUNNER_BIN" "$NAK_BIN" >"$FIXTURE/runners.env"

# Stub host commands: record argv, and for bats the environment the
# suite would inherit. The driver must never call ssh itself.
for tool in bats flock ssh timeout; do
  cat >"$STUBBIN/$tool" <<EOF
#!/bin/sh
echo "$tool \$*" >>"$CALLS"
EOF
  if [ "$tool" = bats ]; then
    cat >>"$STUBBIN/$tool" <<'EOF'
env | grep -E '^(WYRD_|NAK_|PEER_|RELAY_|STATE_DIR|SSH_KEY|E2E_|GATE_|CORE_|GUEST_)' | sort >>"$CALLS"
EOF
  fi
  chmod +x "$STUBBIN/$tool"
  if [ "$tool" != bats ]; then cp "$STUBBIN/$tool" "$NOBATS/$tool"; chmod +x "$NOBATS/$tool"; fi
done

# run_entry <extra-env...> -- <args...>: run the driver with the
# fixture provision, stub host commands first on PATH. Prints status
# on stdout for assertion; driver stderr flows through.
run_entry() {
  local args=() envs=() seen_dash=0 a
  for a in "$@"; do
    if [ "$seen_dash" = 0 ] && [ "$a" = "--" ]; then seen_dash=1; continue; fi
    if [ "$seen_dash" = 0 ]; then envs+=("$a"); else args+=("$a"); fi
  done
  env -i PATH="$STUBBIN:/usr/bin:/bin" CALLS="$CALLS" \
    GATE_TASK_PROVISION_DIR="$PROVISION_DIR" \
    GATE_BATS_LIB="$FIXTURE/bats-lib" \
    GATE_PARTITION_BIN="$FIXTURE/partition-bin" \
    GATE_RUNNERS_ENV="$FIXTURE/runners.env" \
    "${envs[@]}" \
    bash "$ENTRY" "${args[@]}" 2>"$FIXTURE/stderr"
  echo "status=$?"
}

: >"$CALLS"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=0" ]] || die "happy path exits 0, got: $out ($(cat "$FIXTURE/stderr"))"
grep -q "^bats .*/tests/gate-entry-smoke.bats .*/tests/gate-phase-01.bats .*/tests/gate-phase-02.bats .*/tests/gate-phase-03.bats .*/tests/gate-phase-04.bats .*/tests/gate-phase-05.bats .*/tests/gate-phase-06.bats .*/tests/gate-phase-07.bats .*/tests/gate-phase-08.bats .*/tests/gate-phase-09.bats .*/tests/gate-phase-10.bats$" "$CALLS" || die "driver execs bats on the smoke and phase files"
grep -q "^flock -n 9$" "$CALLS" || die "driver holds fd 9 non-blocking (flock -n 9)"
grep -q "^PEER_O=e2e@10.0.7.11$" "$CALLS" || die "peer-o endpoint assembled from roles+subnet"
grep -q "^PEER_N=e2e@10.0.7.12$" "$CALLS" || die "peer-n endpoint assembled from roles+subnet"
grep -q "^RELAY_URL=ws://10.0.7.10:18761$" "$CALLS" || die "relay url assembled from relay role"
grep -q "^WYRD_BIN=$RUNNER_BIN$" "$CALLS" || die "wyrd runner resolved from the handoff"
grep -q "^NAK_BIN=$NAK_BIN$" "$CALLS" || die "nak runner resolved from the handoff"
grep -q "^GATE_BATS_LIB=$FIXTURE/bats-lib$" "$CALLS" || die "bats lib handed to the suite"
grep -q "^GATE_PARTITION_BIN=$FIXTURE/partition-bin$" "$CALLS" || die "tap primitive handed to the suite"
grep -q "^WYRD_E2E_LOCK=$STATE_DIR/lock$" "$CALLS" || die "provision lock handed to the suite"
grep -q "^E2E_INTERVAL=$" "$CALLS" || die "interval default handed to the suite (empty)"
grep -q "^CORE_ONLY=1,2,3,4,5$" "$CALLS" || die "core depth defaults to the five-step core"
grep -q "^GUEST_ENV=/mnt/wyrd-state/run/e2e-env.sh$" "$CALLS" || die "guest env path handed to the suite (host RUN translated)"
token_env="$(grep '^WYRD_E2E_TOKEN=' "$CALLS" | cut -d= -f2-)"
token_file="$(grep '^WYRD_E2E_TOKEN=' "$RUN_DIR/e2e-env.sh" | cut -d= -f2-)"
[[ -n "$token_env" && "$token_env" == "$token_file" ]] || die "run token handed to the suite matches the published file"
grep -q "^WYRD_E2E_PID=[0-9][0-9]*$" "$RUN_DIR/e2e-env.sh" || die "owner pid published in the env file"
[ -z "$(grep "^ssh " "$CALLS" || true)" ] || die "driver dials nothing itself (smoke does)"
pass "happy path assembles env, holds lock, execs bats"

out="$(run_entry --)"
[[ "$out" == "status=2" ]] || die "missing arg refuses with usage (exit 2), got: $out"
out="$(run_entry -- --keep)"
[[ "$out" == "status=2" ]] || die "unknown arg refuses with usage (exit 2), got: $out"
pass "arg discipline: only --fresh"

out="$(env -i PATH="$STUBBIN:/usr/bin:/bin" GATE_BATS_LIB="$FIXTURE/bats-lib" GATE_RUNNERS_ENV="$FIXTURE/runners.env" bash "$ENTRY" --fresh 2>/dev/null; echo "status=$?")"
[[ "$out" == "status=1" ]] || die "missing provision dir refuses, got: $out"
pass "missing GATE_TASK_PROVISION_DIR refuses"

out="$(env -i PATH="$STUBBIN:/usr/bin:/bin" CALLS="$CALLS" GATE_TASK_PROVISION_DIR="$PROVISION_DIR" GATE_BATS_LIB="$FIXTURE/bats-lib" GATE_RUNNERS_ENV="$FIXTURE/runners.env" bash "$ENTRY" --fresh 2>/dev/null; echo "status=$?")"
[[ "$out" == "status=1" ]] || die "missing tap primitive refuses, got: $out"
pass "missing GATE_PARTITION_BIN refuses"

printf 'nak=%s\n' "$NAK_BIN" >"$FIXTURE/runners-nowyrd.env"
out="$(run_entry GATE_RUNNERS_ENV="$FIXTURE/runners-nowyrd.env" -- --fresh)"
[[ "$out" == "status=1" ]] || die "handoff without wyrd refuses, got: $out"
pass "runner handoff without wyrd refuses"

mv "$STATE_DIR/sshkey" "$STATE_DIR/sshkey.away"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "missing ssh key refuses, got: $out"
mv "$STATE_DIR/sshkey.away" "$STATE_DIR/sshkey"
pass "missing ssh key refuses"

printf '' >"$PROVISION_DIR/lock-file"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "empty lock-file datum refuses, got: $out"
printf '%s' "$STATE_DIR/lock" >"$PROVISION_DIR/lock-file"
pass "empty lock-file datum refuses"

out="$(env -i PATH="$NOBATS:/usr/bin:/bin" GATE_TASK_PROVISION_DIR="$PROVISION_DIR" GATE_BATS_LIB="$FIXTURE/bats-lib" GATE_RUNNERS_ENV="$FIXTURE/runners.env" bash "$ENTRY" --fresh 2>/dev/null; echo "status=$?")"
[[ "$out" == "status=1" ]] || die "missing bats refuses, got: $out"
pass "missing bats refuses"

printf 'abc' >"$PROVISION_DIR/relay-port"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "non-numeric relay port refuses, got: $out"
printf '18761' >"$PROVISION_DIR/relay-port"
printf '10.0' >"$PROVISION_DIR/subnet"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "truncated subnet refuses, got: $out"
printf '10.0.7' >"$PROVISION_DIR/subnet"
pass "malformed datums refuse"

# A glob character in a role field must stay literal: under word
# splitting it would expand against the unit's working directory and
# corrupt the field count. The runner field is unused downstream, so
# a `*` there must leave the happy path green.
cp "$PROVISION_DIR/roles" "$FIXTURE/roles.clean"
printf 'peer-o tap-o 11 ssh *|peer-n tap-n 12 ssh runner-peer-n|relay tap-r 10 - runner-relay' >"$PROVISION_DIR/roles"
: >"$CALLS"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=0" ]] || die "glob in role field breaks parsing, got: $out ($(cat "$FIXTURE/stderr"))"
grep -q "^PEER_O=e2e@10.0.7.11$" "$CALLS" || die "peer-o still resolves with a glob runner"
cp "$FIXTURE/roles.clean" "$PROVISION_DIR/roles"
pass "role fields are never glob-expanded"

printf 'peer-o tap-o 999 ssh runner-peer-o|peer-n tap-n 12 ssh runner-peer-n|relay tap-r 10 - runner-relay' >"$PROVISION_DIR/roles"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "out-of-range role octet refuses, got: $out"
cp "$FIXTURE/roles.clean" "$PROVISION_DIR/roles"
pass "role octet range refuses"

printf '10.0.7.999' >"$PROVISION_DIR/subnet"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "out-of-range subnet octet refuses, got: $out"
printf '10.0.7' >"$PROVISION_DIR/subnet"
pass "subnet octet range refuses"

echo "gate-entry: $PASS checks passed"
