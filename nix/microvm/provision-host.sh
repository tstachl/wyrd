#!/usr/bin/env bash
# nix/microvm/provision-host.sh — the privileged half of the e2e run.
#
# Sourced, never executed. Effectful functions over explicit
# arguments: bridge, taps, daemon spawn and teardown, boot waits. No
# side effects at source time. Requires `die` from the sourcer
# (run-microvm.sh and tests/test-provision-host.sh each define their
# own): refusals print `FAIL: ...` to stderr and return nonzero.
#
# Boundary (pinned by tests/test-provision-host.sh): nothing here
# reads the repository, builds anything, or runs the suite. The
# module does not know what a checkout is: no revision reads, no
# builds, no privilege changes, no suite scripts. Runners enter
# only as /nix/store paths naming identified closures; a
# checkout-relative runner is refused before any privileged
# operation. This file is the future host-owned unit: vendored into
# the host configuration and executed from the store, never from a
# mutable checkout. The caller holds the
# host-global run lock, runs as root, and has checked the tools
# (ip, ss, timeout, ssh, ssh-keygen, pkill, pgrep, ps, kill, chown).
#
# Wyrd-suite assumptions that become task capability parameters at
# the gate move (not constants of a shared provisioner): the
# `wyrd-*-virtiofs` socket pattern, the `e2e@` guest user, and the
# ssh-enabled role list. They are constants here because this file
# currently provisions exactly one task.
#
# Host network facts live here, not in the caller: tap names, the
# bridge and the subnet are host-global (two runs under different
# state dirs still share them), while the guest-side addresses stay
# in the guest Nix declarations (pinned to agree by
# tests/test-topology-toml.sh and tests/test-provision-host.sh).

PROV_BRIDGE="br-wyrd"
PROV_NET="10.0.7"
PROV_RELAY_PORT="18761"
PROV_ROLES=(peer-o peer-n relay)
PROV_SSH_ROLES=(peer-o peer-n)
declare -A PROV_TAP=( [peer-o]=tap-o [peer-n]=tap-n [relay]=tap-r )
declare -A PROV_IP=( [peer-o]=11 [peer-n]=12 [relay]=10 )
# Unique string in every daemon cmdline started here (virtiofsd
# --socket-path and qemu -chardev path). PID files cannot work: $!
# is the spawner subshell (dead on arrival), and kept runs are
# strangers to the next invocation. So both the startup pre-clean
# and teardown kill by this pattern, which cannot match anything but
# our own daemons.
PROV_SOCK_PAT='wyrd-.*-virtiofs-wyrd-state\.sock'

# provision_kill_stale_daemons: reap kept runs, killed ssh sessions,
# and supervisor restart loops. Idempotent: a no-op when nothing is
# running.
provision_kill_stale_daemons() {
  local pids pid ppid
  pids="$(pgrep -f "$PROV_SOCK_PAT" || true)"
  [[ -z "$pids" ]] && return 0
  # Supervisors first (the parents): SIGTERM stops their children
  # cleanly instead of orphaning them into a restart loop.
  for pid in $pids; do
    ppid="$(ps -o ppid= -p "$pid" 2>/dev/null || true)"
    ppid="$(echo "$ppid" | tr -d ' ')"
    if [[ -n "$ppid" && "$ppid" != 1 && "$ppid" != "$$" ]]; then
      kill "$ppid" 2>/dev/null || true
    fi
  done
  sleep 2
  pkill -f "$PROV_SOCK_PAT" 2>/dev/null || true
  sleep 1
  # Escalate: a surviving daemon (wedged qemu, respawning
  # supervisor) must not be reported as removed below.
  pids="$(pgrep -f "$PROV_SOCK_PAT" || true)"
  [[ -z "$pids" ]] || pkill -KILL -f "$PROV_SOCK_PAT" 2>/dev/null || true
  sleep 1
  pids="$(pgrep -f "$PROV_SOCK_PAT" || true)"
  [[ -z "$pids" ]] || { echo "  FAIL: stale daemons survive SIGKILL: $pids" >&2; return 1; }
  return 0
}

# provision_teardown <keep>: stop daemons, delete taps and the
# bridge — unless keep is 1, in which case everything stays up for
# debugging. State dirs and logs are retained either way.
provision_teardown() { # <keep: 0|1>
  local keep="$1"
  if [[ "$keep" == 0 ]]; then
    provision_kill_stale_daemons
    local t
    for t in "${PROV_TAP[@]}"; do ip link del "$t" 2>/dev/null || true; done
    unset t
    ip link del "$PROV_BRIDGE" 2>/dev/null || true
  else
    echo "kept: VMs, taps, and $PROV_BRIDGE left up (--keep)"
  fi
}

# provision_teardown_kept: standalone teardown for kept runs. Verifies
# instead of claiming: a surviving guest with a deleted NIC is worse
# than a loud failure. Silent on success; the caller reports.
provision_teardown_kept() {
  local t
  provision_teardown 0
  for t in "${PROV_TAP[@]}"; do
    if ip link show "$t" &>/dev/null; then die "tap $t survives teardown"; fi
  done
  unset t
  if ip link show "$PROV_BRIDGE" &>/dev/null; then die "$PROV_BRIDGE survives teardown"; fi
  return 0
}

# provision_prepare_state <run> <work> <build-logs> <ssh-key>
# <state-dir> <fresh: 0|1>: create the state tree, optionally wipe
# the run, ensure the ssh key and the published authorized_keys, and
# hand the run tree to the guest account (uid 1000, the only normal
# user — numeric, no host user needed). The key stays root-owned.
provision_prepare_state() { # <run> <work> <build-logs> <ssh-key> <state-dir> <fresh>
  local run="$1" work="$2" build_logs="$3" ssh_key="$4" state_dir="$5" fresh="$6"
  mkdir -p "$run" "$run/logs" "$work" "$build_logs"
  [[ "$fresh" == 0 ]] || { rm -rf "$run"; mkdir -p "$run" "$run/logs"; }
  chown -R 1000:1000 "$run"
  [[ -f "$ssh_key" ]] || ssh-keygen -t ed25519 -N "" -f "$ssh_key" -q
  cp "$ssh_key.pub" "$state_dir/authorized_keys.pub"
}

# provision_network: raise the bridge (idempotent — a kept run's
# bridge survives, and re-adding is refused silently, not loudly).
provision_network() {
  ip link add "$PROV_BRIDGE" type bridge 2>/dev/null || true
  ip addr add "$PROV_NET.1/24" dev "$PROV_BRIDGE" 2>/dev/null || true
  ip link set "$PROV_BRIDGE" up
}

# provision_boot_role <role> <runner-store-path> <work-dir>: attach
# the tap, link the identified runner closure, boot virtiofsd (wait
# for LISTEN, not mere existence — the file can appear before
# virtiofsd accepts, and qemu fails fast on refused) then qemu. The
# runner must be a /nix/store path: the provisioner boots identified
# closures, never the checkout.
provision_boot_role() { # <role> <runner-store-path> <work-dir>
  local role="$1" runner="$2" work="$3"
  [[ -n "${PROV_TAP[$role]:-}" ]] || die "unknown role: $role (want one of ${PROV_ROLES[*]})"
  [[ "$runner" == /nix/store/* ]] \
    || die "runner for $role is not a store path: $runner (the provisioner boots identified closures, never the checkout)"
  [[ -x "$runner/bin/virtiofsd-run" ]] \
    || die "$role runner has no virtiofsd-run: $runner"
  [[ -x "$runner/bin/microvm-run" ]] \
    || die "$role runner has no microvm-run: $runner"
  local tap="${PROV_TAP[$role]}" vmdir="$work/vm-$role" i
  ip link del "$tap" 2>/dev/null || true
  # multi_queue: the microvm runner attaches with queues=<vcpu>
  # (multiqueue); a single-queue tap fails the attach with EINVAL
  # ("could not configure /dev/net/tun") and the guest never boots.
  ip tuntap add dev "$tap" mode tap multi_queue
  ip link set "$tap" up
  ip link set "$tap" master "$PROV_BRIDGE"
  mkdir -p "$vmdir"
  ln -sfn "$runner" "$vmdir/runner"
  # Stale sockets from a kept run must go: a leftover .sock passes
  # the readiness check below while no daemon listens, and qemu
  # then dies with "Connection refused".
  rm -f "$vmdir"/*.sock
  # 9>&-: the lock fd must not leak into daemons. Kept daemons
  # outlive the runner, and an inherited lock fd would hold the
  # flock forever: no second run could ever start, and teardown
  # could never acquire the lock it needs to kill them.
  ( cd "$vmdir" \
    && ./runner/bin/virtiofsd-run > virtiofsd.log 2>&1 & ) 9>&-
  # Wait for the socket to LISTEN, not merely exist.
  for i in $(seq 1 30); do
    ss -xl 2>/dev/null | grep -q "wyrd-$role-virtiofs-wyrd-state.sock" && break
    [[ "$i" == 30 ]] && { tail -n 5 "$vmdir/virtiofsd.log"; die "$role: virtiofsd never listened"; }
    sleep 1
  done
  ( cd "$vmdir" && ./runner/bin/microvm-run > qemu.log 2>&1 & ) 9>&-
  echo "    $role up (tap $tap)"
}

# provision_wait_ssh <ssh-key>: wait for the ssh-enabled roles to
# accept connections. Fail fast with the real cause instead of an
# opaque convergence timeout deep in the suite.
provision_wait_ssh() { # <ssh-key>
  local ssh_key="$1" role ip i
  local -a ssh=(ssh -i "$ssh_key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o BatchMode=yes)
  for role in "${PROV_SSH_ROLES[@]}"; do
    ip="${PROV_IP[$role]}"
    for i in $(seq 1 60); do
      "${ssh[@]}" "e2e@$PROV_NET.$ip" true 2>/dev/null && break
      [[ "$i" == 60 ]] && die "e2e@$PROV_NET.$ip ($role) never accepted ssh"
      sleep 2
    done
    echo "    e2e@$PROV_NET.$ip reachable"
  done
}

# provision_wait_relay: wait for the relay control plane before the
# suite — the first relay dial happens deep in the legs, where a
# slow relay boot surfaces as an opaque convergence timeout.
provision_wait_relay() {
  local i relay_ip="${PROV_IP[relay]}"
  for i in $(seq 1 30); do
    timeout 2 bash -c "</dev/tcp/$PROV_NET.$relay_ip/$PROV_RELAY_PORT" 2>/dev/null && break
    [[ "$i" == 30 ]] && die "relay never listened on $PROV_NET.$relay_ip:$PROV_RELAY_PORT"
    sleep 2
  done
  echo "    relay control plane reachable"
}
