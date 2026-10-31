#!/usr/bin/env bash
# tests/harness/test-gate-entry.sh — the gate-path entry assembles the suite
# environment from host-published inputs and refuses bad ones.
#
# Pure bash + stubbed host commands (bats, flock, timeout): no root,
# no nix, no guest. jq is real (the entry parses the image manifests
# with it). The stubs record their argv and environment instead of
# acting, so every assertion below pins the driver's contract without
# touching a VM. Run it anywhere:
#   bash tests/harness/test-gate-entry.sh
#
# The smoke suite itself (tests/gate/gate-entry-smoke.bats) is Odin-only:
# it dials real peers over the agent. This file pins everything up to
# exec bats.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
ENTRY="$ROOT/tests/gate/gate-entry.sh"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

FIXTURE="$(mktemp -d)"
trap 'rm -f "$FIXTURE/stderr"; rm -rf "$FIXTURE"' EXIT
PROVISION_DIR="$FIXTURE/provision"
STATE_DIR="$FIXTURE/state"
RUN_DIR="$STATE_DIR/run"
STUBBIN="$FIXTURE/stubbin"
NOBATS="$FIXTURE/nobats"
CALLS="$FIXTURE/calls"
mkdir -p "$PROVISION_DIR/manifests" "$RUN_DIR" "$STUBBIN" "$NOBATS"

printf 'br-wyrd' >"$PROVISION_DIR/bridge"
printf '18761' >"$PROVISION_DIR/relay-port"
printf '%s' "$STATE_DIR" >"$PROVISION_DIR/state-dir"
printf '%s' "$STATE_DIR/lock" >"$PROVISION_DIR/lock-file"
printf 'relay' >"$PROVISION_DIR/relay-role"
printf 'img-peer-o images.peer-o|img-peer-n images.peer-n|img-relay images.relay' >"$PROVISION_DIR/builds"
printf '{"kind":"gate.image/v1","role":"peer-o","vcpu":4,"memMb":4096,"mac":"02:00:00:00:07:11","ip":"10.0.7.11/24","shares":[]}' >"$PROVISION_DIR/manifests/img-peer-o.json"
printf '{"kind":"gate.image/v1","role":"peer-n","vcpu":4,"memMb":4096,"mac":"02:00:00:00:07:12","ip":"10.0.7.12/24","shares":[]}' >"$PROVISION_DIR/manifests/img-peer-n.json"
printf '{"kind":"gate.image/v1","role":"relay","vcpu":2,"memMb":1024,"mac":"02:00:00:00:07:10","ip":"10.0.7.10/24","shares":[]}' >"$PROVISION_DIR/manifests/img-relay.json"
mkdir -p "$FIXTURE/bats-lib"

# Stub guest transport: records argv, succeeds. The entry only needs
# exit statuses from pre-clean and pushes, never guest output.
cat >"$FIXTURE/guest-bin" <<EOF
#!/bin/sh
echo "guest \$*" >>"$CALLS"
exit 0
EOF
chmod +x "$FIXTURE/guest-bin"

# Stub host commands: record argv, and for bats the environment the
# suite would inherit. The driver must never dial a guest itself.
# sleep is stubbed instant: the lock-acquire grace below must run fast
# when flock never succeeds.
for tool in bats flock timeout sleep; do
  cat >"$STUBBIN/$tool" <<EOF
#!/bin/sh
echo "$tool \$*" >>"$CALLS"
EOF
  if [ "$tool" = bats ]; then
    cat >>"$STUBBIN/$tool" <<'EOF'
env | grep -E '^(WYRD_|NAK_|RELAY_|STATE_DIR|E2E_|GATE_|CORE_|GUEST_)' | sort >>"$CALLS"
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
    GATE_GUEST_BIN="$FIXTURE/guest-bin" \
    "${envs[@]}" \
    bash "$ENTRY" "${args[@]}" 2>"$FIXTURE/stderr"
  echo "status=$?"
}

: >"$CALLS"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=0" ]] || die "happy path exits 0, got: $out ($(cat "$FIXTURE/stderr"))"
# Default suite (no --step): the whole prefix — smoke, core, all
# eleven phases, epilogue.
grep -q "^bats .*/tests/gate/gate-entry-smoke.bats .*/tests/gate/gate-phase-01.bats .*/tests/gate/gate-phase-02.bats .*/tests/gate/gate-phase-03.bats .*/tests/gate/gate-phase-04.bats .*/tests/gate/gate-phase-05.bats .*/tests/gate/gate-phase-06.bats .*/tests/gate/gate-phase-07.bats .*/tests/gate/gate-phase-08.bats .*/tests/gate/gate-phase-09.bats .*/tests/gate/gate-phase-10.bats .*/tests/gate/gate-phase-11.bats .*/tests/gate/gate-epilogue.bats$" "$CALLS" || die "driver execs bats on the smoke and phase files"
grep -q "^flock -n 9$" "$CALLS" || die "driver holds fd 9 non-blocking (flock -n 9)"
grep -q "^RELAY_URL=ws://10.0.7.10:18761$" "$CALLS" || die "relay url assembled from the relay manifest role"
grep -q "^WYRD_BIN=/run/current-system/sw/bin/wyrd$" "$CALLS" || die "wyrd resolves to the stable in-image path"
grep -q "^NAK_BIN=/run/current-system/sw/bin/nak$" "$CALLS" || die "nak resolves to the stable in-image path"
grep -q "^GATE_BATS_LIB=$FIXTURE/bats-lib$" "$CALLS" || die "bats lib handed to the suite"
grep -q "^GATE_GUEST_BIN=$FIXTURE/guest-bin$" "$CALLS" || die "guest transport handed to the suite"
grep -q "^GATE_PARTITION_BIN=$FIXTURE/partition-bin$" "$CALLS" || die "partition primitive handed to the suite"
grep -q "^WYRD_E2E_LOCK=$STATE_DIR/lock$" "$CALLS" || die "provision lock handed to the suite"
grep -q "^E2E_INTERVAL=$" "$CALLS" || die "interval default handed to the suite (empty)"
grep -q "^CORE_ONLY=1,2,3,4,5$" "$CALLS" || die "core depth defaults to the five-step core"
grep -q "^GUEST_ENV=/var/lib/wyrd-e2e/e2e-env.sh$" "$CALLS" || die "guest env path handed to the suite"
grep -q "^GUEST_RUN=/var/lib/wyrd-e2e$" "$CALLS" || die "guest run root handed to the suite"
token_env="$(grep '^WYRD_E2E_TOKEN=' "$CALLS" | cut -d= -f2-)"
token_file="$(grep '^WYRD_E2E_TOKEN=' "$RUN_DIR/e2e-env.sh" | cut -d= -f2-)"
[[ -n "$token_env" && "$token_env" == "$token_file" ]] || die "run token handed to the suite matches the published file"
grep -q "^WYRD_E2E_PID=[0-9][0-9]*$" "$RUN_DIR/e2e-env.sh" || die "owner pid published in the env file"
# Guest-required keys: the guest legs consume this file, so every key
# they need must be present with gate-path values.
grep -q "^WYRD_BIN=/run/current-system/sw/bin/wyrd$" "$RUN_DIR/e2e-env.sh" || die "wyrd path published in the env file"
grep -q "^NAK_BIN=/run/current-system/sw/bin/nak$" "$RUN_DIR/e2e-env.sh" || die "nak path published in the env file"
grep -q "^RELAY_HOST=10.0.7.10$" "$RUN_DIR/e2e-env.sh" || die "relay host published in the env file"
grep -q "^RELAY_PORT=18761$" "$RUN_DIR/e2e-env.sh" || die "relay port published in the env file"
grep -q "^RELAY_MANAGED=0$" "$RUN_DIR/e2e-env.sh" || die "relay unmanaged in the env file (host-provisioned)"
grep -q "^CHECKOUT=/etc/wyrd-tests$" "$RUN_DIR/e2e-env.sh" || die "guest tests path published in the env file"
grep -q "^E2E_ROOT=/var/lib/wyrd-e2e$" "$RUN_DIR/e2e-env.sh" || die "guest run root published in the env file"
grep -q "^WYRD_PEER_O_IP=10.0.7.11$" "$RUN_DIR/e2e-env.sh" || die "peer-o ip published in the env file"
grep -q "^WYRD_PEER_N_IP=10.0.7.12$" "$RUN_DIR/e2e-env.sh" || die "peer-n ip published in the env file"
grep -q "^RENDEZVOUS_PORT=18770$" "$RUN_DIR/e2e-env.sh" || die "rendezvous port published in the env file"
# Guest preparation rides the agent before the suite: fresh state
# roots on both peers, then the env file where the legs read it.
# (Through the timeout wrapper the entry bounds every agent call with.)
grep -q "guest-bin exec peer-o -- mkdir -p /var/lib/wyrd-e2e$" "$CALLS" || die "peer-o state root prepared"
grep -q "guest-bin exec peer-o -- chown -R e2e:users /var/lib/wyrd-e2e$" "$CALLS" || die "peer-o state root handed to the suite user"
grep -q "guest-bin exec peer-n -- mkdir -p /var/lib/wyrd-e2e$" "$CALLS" || die "peer-n state root prepared"
grep -q "guest-bin push peer-o $RUN_DIR/e2e-env.sh /var/lib/wyrd-e2e/e2e-env.sh$" "$CALLS" || die "env pushed to peer-o"
grep -q "guest-bin push peer-n $RUN_DIR/e2e-env.sh /var/lib/wyrd-e2e/e2e-env.sh$" "$CALLS" || die "env pushed to peer-n"
[ -z "$(grep -E "^(ssh|scp) " "$CALLS" || true)" ] || die "driver dials nothing itself (agent only)"
[ -z "$(grep "PEER_O=" "$CALLS" || true)" ] || die "driver assembles no ssh-style peer endpoints"
pass "happy path assembles env, prepares guests, holds lock, execs bats"

out="$(run_entry --)"
[[ "$out" == "status=2" ]] || die "missing arg refuses with usage (exit 2), got: $out"
out="$(run_entry -- --keep)"
[[ "$out" == "status=2" ]] || die "unknown arg refuses with usage (exit 2), got: $out"
out="$(run_entry -- --fresh --step)"
[[ "$out" == "status=2" ]] || die "bare --step refuses with usage (exit 2), got: $out"
out="$(run_entry -- --fresh --step 99)"
[[ "$out" == "status=1" ]] || die "out-of-range step refuses (exit 1), got: $out"
out="$(run_entry -- --fresh --step 6,banana)"
[[ "$out" == "status=1" ]] || die "malformed step refuses (exit 1), got: $out"
pass "arg discipline: only --fresh, --step takes a 1-15 comma list"

: >"$CALLS"
out="$(run_entry -- --fresh --step 6)"
[[ "$out" == "status=0" ]] || die "step run exits 0, got: $out ($(cat "$FIXTURE/stderr"))"
grep -q "^bats .*/tests/gate/gate-entry-smoke.bats .*/tests/gate/gate-phase-01.bats .*/tests/gate/gate-phase-02.bats .*/tests/gate/gate-epilogue.bats$" "$CALLS" || die "step 6 runs the 1..6 prefix (smoke, core, member-n, epilogue)"
grep -q "^CORE_ONLY=1,2,3,4,5$" "$CALLS" || die "step 6 keeps the full five-step core"

: >"$CALLS"
out="$(run_entry -- --fresh --step 1,3)"
[[ "$out" == "status=0" ]] || die "step run exits 0, got: $out ($(cat "$FIXTURE/stderr"))"
grep -q "^bats .*/tests/gate/gate-entry-smoke.bats .*/tests/gate/gate-phase-01.bats .*/tests/gate/gate-epilogue.bats$" "$CALLS" || die "steps 1,3 truncate to the core prefix (smoke, core, epilogue)"
grep -q "^CORE_ONLY=1,2,3$" "$CALLS" || die "core depth truncates to the highest selected core step"
pass "step selection runs the prefix closure with a truncated core"

: >"$CALLS"
out="$(run_entry -- --fresh --step 4)"
[[ "$out" == "status=0" ]] || die "step run exits 0, got: $out ($(cat "$FIXTURE/stderr"))"
grep -q "^bats .*/tests/gate/gate-entry-smoke.bats .*/tests/gate/gate-phase-01.bats .*/tests/gate/gate-epilogue.bats$" "$CALLS" || die "step 4 runs core-only (smoke, core, epilogue)"
grep -q "^CORE_ONLY=1,2,3,4$" "$CALLS" || die "core depth truncates to step 4"

: >"$CALLS"
out="$(run_entry -- --fresh --step 7,9)"
[[ "$out" == "status=0" ]] || die "step run exits 0, got: $out ($(cat "$FIXTURE/stderr"))"
grep -q "^bats .*/tests/gate/gate-entry-smoke.bats .*/tests/gate/gate-phase-01.bats .*/tests/gate/gate-phase-02.bats .*/tests/gate/gate-phase-03.bats .*/tests/gate/gate-phase-04.bats .*/tests/gate/gate-phase-05.bats .*/tests/gate/gate-epilogue.bats$" "$CALLS" || die "steps 7,9 run the 1..9 prefix (smoke, phases 01-05, epilogue)"
grep -q "^CORE_ONLY=1,2,3,4,5$" "$CALLS" || die "core stays full once the prefix reaches step 5"
pass "step matrix covers core-only and mid-range multi-selections"

out="$(env -i PATH="$STUBBIN:/usr/bin:/bin" GATE_BATS_LIB="$FIXTURE/bats-lib" GATE_GUEST_BIN="$FIXTURE/guest-bin" bash "$ENTRY" --fresh 2>/dev/null; echo "status=$?")"
[[ "$out" == "status=1" ]] || die "missing provision dir refuses, got: $out"
pass "missing GATE_TASK_PROVISION_DIR refuses"

out="$(env -i PATH="$STUBBIN:/usr/bin:/bin" CALLS="$CALLS" GATE_TASK_PROVISION_DIR="$PROVISION_DIR" GATE_BATS_LIB="$FIXTURE/bats-lib" GATE_GUEST_BIN="$FIXTURE/guest-bin" bash "$ENTRY" --fresh 2>/dev/null; echo "status=$?")"
[[ "$out" == "status=1" ]] || die "missing partition primitive refuses, got: $out"
pass "missing GATE_PARTITION_BIN refuses"

out="$(env -i PATH="$STUBBIN:/usr/bin:/bin" CALLS="$CALLS" GATE_TASK_PROVISION_DIR="$PROVISION_DIR" GATE_BATS_LIB="$FIXTURE/bats-lib" GATE_PARTITION_BIN="$FIXTURE/partition-bin" bash "$ENTRY" --fresh 2>/dev/null; echo "status=$?")"
[[ "$out" == "status=1" ]] || die "missing guest transport refuses, got: $out"
pass "missing GATE_GUEST_BIN refuses"

printf '#!/bin/sh\nexit 0\n' >"$FIXTURE/guest-notexec"
chmod 644 "$FIXTURE/guest-notexec"
out="$(env -i PATH="$STUBBIN:/usr/bin:/bin" CALLS="$CALLS" GATE_TASK_PROVISION_DIR="$PROVISION_DIR" GATE_BATS_LIB="$FIXTURE/bats-lib" GATE_PARTITION_BIN="$FIXTURE/partition-bin" GATE_GUEST_BIN="$FIXTURE/guest-notexec" bash "$ENTRY" --fresh 2>/dev/null; echo "status=$?")"
[[ "$out" == "status=1" ]] || die "non-executable guest transport refuses, got: $out"
pass "non-executable GATE_GUEST_BIN refuses"

mv "$PROVISION_DIR/manifests" "$FIXTURE/manifests.away"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "missing manifests dir refuses, got: $out"
mv "$FIXTURE/manifests.away" "$PROVISION_DIR/manifests"
pass "missing manifests dir refuses"

mv "$PROVISION_DIR/manifests/img-peer-o.json" "$FIXTURE/peer-o.away"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "manifests without peer-o refuse, got: $out"
mv "$FIXTURE/peer-o.away" "$PROVISION_DIR/manifests/img-peer-o.json"
pass "manifests without peer-o refuse"

printf 'member' >"$PROVISION_DIR/relay-role"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "relay role naming no manifest refuses, got: $out"
printf 'relay' >"$PROVISION_DIR/relay-role"
pass "relay role naming no manifest refuses"

printf '{"kind":"gate.image/v1","role":"peer-o"' >"$PROVISION_DIR/manifests/img-peer-o.json"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "malformed manifest refuses, got: $out"
printf '{"kind":"gate.image/v1","role":"peer-o","vcpu":4,"memMb":4096,"mac":"02:00:00:00:07:11","ip":"10.0.7.11/24","shares":[]}' >"$PROVISION_DIR/manifests/img-peer-o.json"
pass "malformed manifest refuses"

printf '{"kind":"gate.image/v1","role":"peer-o","vcpu":4,"memMb":4096,"mac":"02:00:00:00:07:11","ip":"10.0.999.11/24","shares":[]}' >"$PROVISION_DIR/manifests/img-peer-o.json"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "out-of-bounds manifest ip refuses, got: $out"
printf '{"kind":"gate.image/v1","role":"peer-o","vcpu":4,"memMb":4096,"mac":"02:00:00:00:07:11","ip":"10.0.7.11/24","shares":[]}' >"$PROVISION_DIR/manifests/img-peer-o.json"
pass "out-of-bounds manifest ip refuses"

printf '' >"$PROVISION_DIR/lock-file"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "empty lock-file datum refuses, got: $out"
printf '%s' "$STATE_DIR/lock" >"$PROVISION_DIR/lock-file"
pass "empty lock-file datum refuses"

out="$(env -i PATH="$NOBATS:/usr/bin:/bin" GATE_TASK_PROVISION_DIR="$PROVISION_DIR" GATE_BATS_LIB="$FIXTURE/bats-lib" GATE_GUEST_BIN="$FIXTURE/guest-bin" bash "$ENTRY" --fresh 2>/dev/null; echo "status=$?")"
[[ "$out" == "status=1" ]] || die "missing bats refuses, got: $out"
pass "missing bats refuses"

printf 'abc' >"$PROVISION_DIR/relay-port"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "non-numeric relay port refuses, got: $out"
printf '18761' >"$PROVISION_DIR/relay-port"
printf '99999' >"$PROVISION_DIR/relay-port"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "out-of-range relay port refuses, got: $out"
printf '18761' >"$PROVISION_DIR/relay-port"
pass "malformed relay port refuses"

printf 'member relay with spaces' >"$PROVISION_DIR/relay-role"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=1" ]] || die "out-of-bounds relay role refuses, got: $out"
printf 'relay' >"$PROVISION_DIR/relay-role"
pass "out-of-bounds relay role refuses"

# Transient holder (the launcher releasing its hold as the unit
# starts): the first acquires fail, then one succeeds. The entry
# waits out the handoff instead of refusing a healthy run.
cat >"$STUBBIN/flock" <<EOF
#!/bin/sh
echo "flock \$*" >>"$CALLS"
n="\$(cat "$FIXTURE/flock-count" 2>/dev/null || echo 0)"
n=\$((n + 1))
echo "\$n" >"$FIXTURE/flock-count"
if [ "\$n" -lt 3 ]; then exit 1; fi
exit 0
EOF
chmod +x "$STUBBIN/flock"
: >"$CALLS"; rm -f "$FIXTURE/flock-count"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=0" ]] || die "transient lock holder breaks the run, got: $out ($(cat "$FIXTURE/stderr"))"
[[ "$(grep -c "^flock -n 9$" "$CALLS")" == "3" ]] || die "entry does not retry the acquire: $(cat "$CALLS")"
pass "transient lock holder is retried, then acquired"

# Persistent holder (a genuinely foreign run): the full grace is
# retried, then the run refuses with the cause named.
cat >"$STUBBIN/flock" <<EOF
#!/bin/sh
echo "flock \$*" >>"$CALLS"
exit 1
EOF
chmod +x "$STUBBIN/flock"
: >"$CALLS"
out="$(run_entry -- --fresh)"
[[ "$out" == "status=2" ]] || die "held lock does not refuse, got: $out"
grep -q "another run holds" "$FIXTURE/stderr" || die "held lock hides the cause"
[[ "$(grep -c "^flock -n 9$" "$CALLS")" == "60" ]] || die "entry does not retry for the full grace"
pass "held lock refuses after the acquire grace"

echo "gate-entry: $PASS checks passed"
