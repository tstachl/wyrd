#!/usr/bin/env bash
# tests/harness/e2e-helpers.bash — guest transport for the gate e2e suite.
#
# Sourced, never executed: functions only, no side effects at source
# time. Sourced by the gate-path bats phases
# (tests/gate/gate-phase-*.bats) and the gate entry smoke suite
# (tests/gate/gate-entry-smoke.bats). The transport is the Incus agent:
# commands run inside the named role's guest and files move over
# `incus file`, through the gate-owned helpers (`load
# "$GATE_BATS_LIB/guest"` in each phase provides guest_exec,
# guest_push, guest_pull; `load "$GATE_BATS_LIB/partition"` provides
# gate_partition). This file adds the suite's routing on top: on_o
# and on_n run legs on peer-o/peer-n with the run token, so every
# leg call site keeps its shape while the transport underneath
# changed from ssh to the agent.
#
# Requires from the environment (published by tests/gate/gate-entry.sh):
# GATE_GUEST_BIN (agent transport), WYRD_BIN (in-image wyrd path),
# WYRD_E2E_TOKEN/LOCK, GUEST_ENV/GUEST_RUN/GUEST_TESTS, RELAY_URL.
# Elevation mirrors the gate-owned bats helpers (GATE_GUEST_BIN,
# GATE_PARTITION_BIN): the unit runs as the build user, so callers
# elevate with -n (fail fast, never prompt), setuid wrapper first.
# Provides task constants with overrideable defaults: GUEST_TESTS
# (baked suite path in guests), GUEST_ENV (guest env file path),
# GUEST_RUN (guest run-tree path).
# host_partition (fault phases) is mandatory and fatal: the host must
# set GATE_PARTITION_BIN. It is role-addressed (gate_partition
# <role> up|down): only the relay is ever partitioned, never a peer.
#
# Also provides the harness failure primitive (die) and the
# host-driven fault rendezvous waiter (wait_guest_file), shared so
# guest-touching waits fail identically. Note the semantic shift: in
# the old host script die aborted the whole run; in bats each @test
# runs in its own process, so die aborts only the current test and
# later phases cascade-fail on the missing state.
#
# Guest-to-guest rendezvous (legs coordinating across peers) does NOT
# live here: with no shared filesystem, legs signal over the test
# bridge (peer_signal/peer_wait in tests/lima/alpha-common.sh),
# and host-observed signals poll the writer guest (wait_guest_file
# below). Host-to-guest signals are plain guest_exec touch calls at
# the call site.

# die <msg>: fail with the harness FAIL line. Gate path: aborts the
# current @test.
die() { echo "  FAIL: $1" >&2; exit 1; }

: "${GUEST_TESTS:=/etc/wyrd-tests}"
: "${GUEST_ENV:=/var/lib/wyrd-e2e/e2e-env.sh}"
: "${GUEST_RUN:=/var/lib/wyrd-e2e}"

# e2e_sudo_bin: print the elevation to use for GATE_GUEST_BIN and
# GATE_PARTITION_BIN. The setuid wrapper lives in the host-controlled
# wrappers dir; prefer it when present, else the PATH fallback. No
# environment crosses the boundary (-n fails fast instead of
# prompting). Mirrors gate-owned guest.bash.
e2e_sudo_bin() {
  if [ -n "${E2E_SUDO_BIN:-}" ]; then printf '%s' "$E2E_SUDO_BIN"; return 0; fi
  if [ -x /run/wrappers/bin/sudo ]; then printf '%s' /run/wrappers/bin/sudo; else printf '%s' sudo; fi # sudo_bin fallback
}

# on_o <cmd...>: run the command on peer-o as the unprivileged suite
# user, carrying the run token (and the explicit-unwrapped flag) so
# the guest-side guard can verify the call came through a wrapper.
# The agent lands as root; as_e2e drops to e2e, mirroring the old
# ssh-as-e2e transport and the host build user (same uid). Runs
# through a remote bash, like the old ssh transport: the legs pass
# env assignments and compound commands as one string.
on_o() {
  as_e2e peer-o bash -c "WYRD_E2E_TOKEN=${WYRD_E2E_TOKEN:-} WYRD_E2E_UNWRAPPED=${WYRD_E2E_UNWRAPPED:-0} $*"
}

# on_n <cmd...>: run the command on peer-n. Same contract as on_o.
on_n() {
  as_e2e peer-n bash -c "WYRD_E2E_TOKEN=${WYRD_E2E_TOKEN:-} WYRD_E2E_UNWRAPPED=${WYRD_E2E_UNWRAPPED:-0} $*"
}

# as_e2e <role> <cmd...>: run the command in the role's guest as the
# unprivileged suite user. Setup/teardown file moves stay root (they
# run before any suite premise applies); everything the suite
# executes — legs and wyrd invocations — routes through here. The
# agent lands as root with a bare lookup path, so the target command
# resolves through an explicit guest PATH (set via absolute env, which
# needs no lookup): bare `bash` and every tool the legs invoke
# resolve in /run/current-system/sw/bin first, mirroring the old
# ssh-as-e2e login path.
as_e2e() { # <role> <cmd...>
  local role="$1"; shift
  if [[ -z "${GATE_GUEST_BIN:-}" ]]; then echo "e2e-helpers: as_e2e requires GATE_GUEST_BIN" >&2; return 1; fi
  guest_exec "$role" runuser -u e2e -g users -G wheel -- /usr/bin/env "PATH=/run/current-system/sw/bin:/usr/bin:/bin" "$@"
}

# guest_timeout <secs> <role> <cmd...>: agent exec with a hard bound.
# timeout cannot execute a shell function, so the bound wraps the
# gate transport binary directly (elevated, like guest_exec) — never
# guest_exec/on_o/on_n (pinned by tests/harness/test-timeout-discipline.sh). The KILL
# grace makes the bound hard: a transport client that ignores TERM
# is killed instead of stretching the call past its budget.
guest_timeout() { # <secs> <role> <cmd...>
  local secs="$1"; shift
  timeout -k 10 "$secs" "$(e2e_sudo_bin)" -n "$GATE_GUEST_BIN" exec "$@"
}

# guest_wyrd <role> <wyrd-args...>: run the in-image wyrd binary in a
# guest as the suite user. WYRD_BIN is the stable in-image path the
# entry publishes; output streams back to the caller (bats `run`
# captures it).
guest_wyrd() { # <role> <wyrd-args...>
  local role="$1"; shift
  if [[ -z "${WYRD_BIN:-}" ]]; then echo "e2e-helpers: guest_wyrd requires WYRD_BIN" >&2; return 1; fi
  as_e2e "$role" "$WYRD_BIN" "$@"
}

# mint_guest_creds <role> <creds-dir> <drive-dir>: create credential
# material with the suite's standard sizes, owned by the suite user
# of the guest that will use it. Secrets never touch the host:
# everything runs in-guest through the agent.
mint_guest_creds() { # <role> <creds-dir> <drive-dir>
  local role="$1" c="$2" d="$3"
  as_e2e "$role" bash -c '
    set -e
    mkdir -p "$0" "$1"
    head -c 32 /dev/urandom | od -An -tx1 | tr -d " \\n" > "$0/identity"
    printf "e2e-%s\\n" "$(head -c 16 /dev/urandom | od -An -tx1 | tr -d " \\n")" > "$0/passphrase"
    chmod 600 "$0/identity" "$0/passphrase"
  ' "$c" "$d"
}

# host_partition <role> down|up: host-driven link surgery for fault
# phases. Mandatory: refuses when the host did not provide
# GATE_PARTITION_BIN.
host_partition() { # <role> down|up
  [[ -n "${GATE_PARTITION_BIN:-}" ]] \
    || die "partition capability not provided (host must set GATE_PARTITION_BIN)"
  "$(e2e_sudo_bin)" -n "$GATE_PARTITION_BIN" "$@"
}

# wait_guest_file <role> <seconds> <guest-path> <what> <leg-pid>:
# shared host-driven rendezvous waiter for guest-touched signals:
# fail fast when the awaited leg exits early instead of burning the
# whole budget on a corpse. Polls at 1 Hz: each attempt is an agent
# round trip, and the budgets are minutes. Returns 0 on rendezvous,
# 1 on timeout or corpse — never exits: callers die (or heal-then-die)
# themselves, so every failure surfaces as a verdict. (An EXIT trap
# must never be used for the heal: it replaces bats' own result
# reporting and turns failures into missing verdicts with a
# passing-looking count.)
wait_guest_file() { # <role> <seconds> <guest-path> <what> <leg-pid>
  local role="$1" budget="$2" f="$3" what="$4" leg="$5" i
  for ((i = 0; i < budget; i++)); do
    guest_exec "$role" test -f "$f" >/dev/null 2>&1 && return 0
    # A dead leg never touches its file: fail fast instead of burning
    # the whole budget on a corpse. The pid is per-file (the sibling
    # leg may already have finished fine); $what names the awaited
    # rendezvous and its leg log.
    kill -0 "$leg" 2>/dev/null || { echo "  FAIL: $what (leg exited early)" >&2; return 1; }
    sleep 1
  done
  echo "  FAIL: $what" >&2
  return 1
}
