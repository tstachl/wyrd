#!/usr/bin/env bash
# nix/microvm/run-microvm.sh — dev-loop runner for the hardened e2e gate.
#
# DEV LOOP ONLY: this file executes repository-controlled code as host
# root (taps, bridge, virtiofsd, builds), which is the developer's own
# trust domain on their own machine. It is never executed on Odin via
# the gate task path — there the host owns the privileged operations
# (host-owned provisioner + capability binaries) and the suite runs
# unprivileged (see tests/alpha-microvm.sh and
# tests/test-no-host-privilege.sh, which pins that boundary).
# Runs on the Linux KVM host as root: qemu tap devices, the
# virtiofsd share daemons, and the bridge all need privilege (the
# microvm.nix virtiofsd wrapper refuses non-root, and taps attach to
# a host bridge). One command end to end:
#
#   sudo ./nix/microvm/run-microvm.sh [--state-dir DIR] [--fresh] [--keep]
#   sudo ./nix/microvm/run-microvm.sh [--state-dir DIR] [--step N[,N...]]
#   sudo ./nix/microvm/run-microvm.sh [--state-dir DIR] [--subject DIR]
#   sudo ./nix/microvm/run-microvm.sh --teardown
#
#   --state-dir DIR  host state root (default /var/lib/wyrd-microvm/state)
#   --fresh          wipe $STATE_DIR/run before booting (default: keep,
#                    so a failed run's drives survive for forensics)
#   --keep           leave VMs and network up afterwards for debugging
#   --step           run suite phases 1..N (comma lists allowed; the run
#                    is the prefix closure, since phases build on each
#                    other: 1-5 are the shared-core Lima steps on peer-o,
#                    6-15 the microvm phases in tests/alpha-microvm.sh)
#   --subject DIR    build and run a host-supplied wyrd source tree
#                    instead of the checkout's own build (plan step 4).
#                    DIR is staged into the per-run work area with
#                    bounded ingestion (256 MiB, 20000 files, .git /
#                    target / result* / .direnv excluded — none enter
#                    evaluation), pinned by host-computed narHash, and
#                    built under the proof preconditions (unprivileged
#                    build user, IFD denied, offline). Combinations with
#                    --teardown are refused. A first subject run on a
#                    cold store fails loudly naming the missing paths:
#                    warm it with a default run first, never by
#                    relaxing the flags.
#   --teardown       stop a kept run (daemons, taps, bridge) and exit;
#                    takes the run lock, so it refuses while a suite
#                    is active. State dirs and logs are retained.
#
# Environment:
#   E2E_BUILD_USER  the account the nix builds run as, named by the
#                    host's suite configuration. The tree's owner is
#                    the fallback when the host names none. Refused by
#                    name: a build that would run as root (by uid, or
#                    by the account's own primary gid), and a build
#                    whose account cannot read the tree it is handed or
#                    write the home the nix client caches in.
#
# Flow: build wyrd + three VM runners -> provision (bridge/taps,
# virtiofsd+qemu per VM from identified store closures, ssh/relay
# waits; nix/microvm/provision-host.sh, the privileged half) ->
# ephemeral ssh key + e2e-env.sh into the state share ->
# tests/alpha-microvm.sh -> report -> teardown (unless --keep).
# The whole suite runs under `timeout` (120 min): a hung guest must
# never wedge the host. Raised from 90 when the crash/exhaustion/
# unknown-parent phases landed. Arithmetic: base phases 1-12 run
# ~20-25 min observed; new-phase typicals sum to ~15-20 min;
# dominant budget products (13b bursts ~61 min, fills ~48 min) are
# flake insurance that returns early and only all fire together in
# pathology. The cap covers typicals plus margin and pathology —
# it is not the sum of worst cases, and an opaque SIGKILL at the
# cap must never be the way a slow phase reports (every phase and
# long loop dies loudly on its own budget first). Each new phase
# states its typical plus dominant products in
# tests/alpha-microvm.sh, and the cap moves only with that
# statement attached. Legs carry their own shorter polls; this
# bound is whole-run pathology cover.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Shared --step grammar (pure function, no traps or side effects — safe
# to source in every mode including --teardown). The host orchestrator
# re-checks with the same function, so a direct
# tests/alpha-microvm.sh invocation fails the same way.
# shellcheck disable=SC1091
source "$SCRIPT_DIR/../../tests/step-grammar.sh"
# Which account the builds run as (pure policy, no side effects — safe
# to source in every mode). The runner and its stub decide the same
# way; the module header says why the host names the account.
# shellcheck disable=SC1091
source "$SCRIPT_DIR/build-account.sh"
# Subject staging (bounded ingestion, hostile-content refusal, pin).
# Pure functions over explicit directories; the test suite exercises
# them without a host. See nix/microvm/stage-subject.sh.
# shellcheck disable=SC1091
source "$SCRIPT_DIR/stage-subject.sh"
# Source-tree resolution (gate-handed vs git-discovered). Pure over
# explicit arguments; the test suite exercises it without a host.
# See nix/microvm/source-root.sh.
# shellcheck disable=SC1091
source "$SCRIPT_DIR/source-root.sh"
# Privileged host operations (bridge, taps, daemon spawn and
# teardown, boot waits) over explicit arguments; no repository reads,
# no builds, no suite. See nix/microvm/provision-host.sh. This is
# the future host-owned unit: the split exists so the host can
# vendor it and stop executing a checkout as root.
# shellcheck disable=SC1091
source "$SCRIPT_DIR/provision-host.sh"

need() { command -v "$1" >/dev/null || { echo "error: missing host tool: $1" >&2; exit 2; }; }

STATE_DIR="/var/lib/wyrd-microvm/state"
FRESH=0
KEEP=0
TEARDOWN=0
ONLY_STEP=""
SUBJECT_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --state-dir) STATE_DIR="$2"; shift 2 ;;
    --fresh) FRESH=1; shift ;;
    --keep) KEEP=1; shift ;;
    --step)
      # An explicit but empty value is a mistake, not "all phases".
      [[ -n "${2:-}" ]] || { echo "error: --step needs at least one step" >&2; exit 2; }
      ONLY_STEP="$2"
      shift 2
      ;;
    --subject)
      # A host-supplied wyrd source tree to test instead of the
      # checkout's own build (plan step 4). Ingestion is bounded and
      # pinned (see the staging block); the override is refused
      # unpinned, never silently.
      [[ -n "${2:-}" ]] || { echo "error: --subject needs a directory" >&2; exit 2; }
      SUBJECT_DIR="$2"
      shift 2
      ;;
    --teardown) TEARDOWN=1; shift ;;
    *) echo "error: unknown flag $1 (see header)" >&2; exit 2 ;;
  esac
done

# Validate --step now, not after the multi-minute build and boot: a
# typo must fail here. The orchestrator re-checks (same grammar), so
# a direct tests/alpha-microvm.sh invocation fails the same way.
if [[ -n "$ONLY_STEP" ]]; then
  step_max 1 15 "$ONLY_STEP" >/dev/null \
    || { echo "error: --step: '$ONLY_STEP' is not a step (expected a comma list of 1-15)" >&2; exit 2; }
fi

# Resolve the subject early (against the invocation cwd, before the
# cd to the checkout below) and refuse a non-directory here, not
# after the multi-minute build and boot.
if [[ -n "$SUBJECT_DIR" ]]; then
  [[ -d "$SUBJECT_DIR" ]] || { echo "error: --subject is not a directory: $SUBJECT_DIR" >&2; exit 2; }
  SUBJECT_DIR="$(cd "$SUBJECT_DIR" && pwd)"
fi

[[ "$EUID" == 0 ]] || { echo "error: run as root (taps, bridge, virtiofsd need it)" >&2; exit 2; }
# The tree the runner builds: the gate-handed revision-bound source
# when launched by the wrapper (GATE_SOURCE), else git discovery for
# the dev loop. A set-but-unusable GATE_SOURCE is refused, never
# worked around — falling back would silently unbind the build.
ROOT="$(resolve_source_root "${GATE_SOURCE:-}")"
cd "$ROOT"

# Unit runs get a minimal closure PATH; fall back to the NixOS system
# profile for everyday tools the suite scripts assume (tar, gzip, ...).
# Append, never prepend: the closure's nix proved itself in the build
# phase and must not be shadowed by system versions.
if [[ -d /run/current-system/sw/bin ]]; then
  export PATH="$PATH:/run/current-system/sw/bin"
fi

# The builds run as a build account, not as the privileged account
# that runs the runner; devices/network stay root. The account must be
# able to build the tree it is handed: nix copies the tree, so the
# account must be able to list it and read its files, and for a git
# checkout nix's own fetch of the repository applies its ownership
# rules on top (the reason the builds ran as the checkout's owner
# until now). The host names the account (E2E_BUILD_USER) and hands
# it a tree it can build; the runner resolves the identity, checks
# that the account can build the tree, and refuses by name when it
# cannot. A build that would run as root is refused, never performed:
# the check is on the resolved uid, so a second name for uid 0 does
# not slip through.
# --teardown builds nothing, so it must not depend on the host's build
# account being configured: it stops a kept run on a host that never
# named one.
if [[ "$TEARDOWN" == 0 ]]; then
  need id
  need setpriv
  need stat
  resolve_build_identity "$ROOT" "${E2E_BUILD_USER:-}" || exit 2
  # Say who the build runs as: a launcher that drops E2E_BUILD_USER
  # (a plain sudo resets the environment) falls back to the tree's
  # owner, and that must be visible in the run log, not inferred.
  # The identity is set by the sourced policy module.
  # shellcheck disable=SC2153
  echo "    build account: $BUILD_USER (uid $BUILD_UID, gid $BUILD_GID${BUILD_GROUP:+ $BUILD_GROUP})"
fi

# Tools per mode, checked after flag parsing so --teardown only
# demands what it uses. Flag combos with --teardown fail loudly
# instead of silently ignoring the rest; a non-default --state-dir
# with --teardown is refused for the same reason (taps, bridge, and
# daemon pattern are all host-global, so it would be ignored).
if [[ "$TEARDOWN" == 1 ]]; then
  [[ "$FRESH" == 0 && "$KEEP" == 0 && -z "$ONLY_STEP" && -z "$SUBJECT_DIR" ]] || { echo "error: --teardown takes no other flags" >&2; exit 2; }
  [[ "$STATE_DIR" == "/var/lib/wyrd-microvm/state" ]] || { echo "error: --teardown ignores --state-dir (host-global)" >&2; exit 2; }
  for t in git pgrep ps kill pkill ip flock; do need "$t"; done
else
  for t in nix ssh ssh-keygen ip ss timeout flock ps pgrep pkill fallocate setpriv nix-store; do need "$t"; done
fi

# One run at a time per host, not per state dir: taps, the bridge,
# and the daemon socket pattern (see provision-host.sh) are all
# host-global, so two runs under different
# --state-dir would still reap each other's daemons and fight over
# br-wyrd. The lock path is fixed for the same reason. Held for the
# whole script via fd 9. The path also travels to the suite as
# WYRD_E2E_LOCK so the orchestrator can prove a wrapper is alive
# (see tests/e2e-guard.sh); keep the literal in one place.
LOCK="/var/lib/wyrd-microvm/lock"
mkdir -p /var/lib/wyrd-microvm
exec 9>"$LOCK"
flock -n 9 || { echo "error: another run holds $LOCK" >&2; exit 2; }

# Per-run ownership token: minted here, published in the guest env
# file, handed to the suite in the environment. Suite scripts compare
# the caller-supplied token against the published one and refuse
# direct execution without it (tests/e2e-guard.sh). Minted hex, so
# ssh env-prefixing needs no quoting; /dev/urandom exists on the
# Linux host this runner requires.
E2E_TOKEN="$(head -c16 /dev/urandom | od -An -tx1 | tr -d ' \n')"

# Host network facts (bridge, subnet, taps) live in
# nix/microvm/provision-host.sh now; the state layout stays here.
RUN="$STATE_DIR/run"
WORK="$STATE_DIR/work"
# Build logs live outside $RUN: phase 1's guest pre-clean empties
# $RUN (the run dir: drives, creds, logs), so anything under
# $RUN/logs is gone post-run. e2e-env.sh, build-logs, and the key
# material beside $RUN are untouched by the wipe.
BUILD_LOGS="$STATE_DIR/build-logs"
SSH_KEY="$STATE_DIR/sshkey"

die() { echo "  FAIL: $1" >&2; exit 1; }

# EXIT trap: stop the suite witness, then hand teardown to the
# provisioner (which kills daemons and, unless --keep, removes taps
# and the bridge). The witness dies first: it appends to $RUN/logs
# on its own cadence and must not outlive the run (a surviving
# logger would write a torn-down run's file, and inherit the lock
# fd — see the 9>&- rule at its launch). LOGGER_PID is unset outside
# a full run (--teardown mode), so the guard defaults empty under
# set -u.
teardown() {
  [[ -n "${LOGGER_PID:-}" ]] && kill "$LOGGER_PID" 2>/dev/null || true
  provision_teardown "$KEEP"
}
trap teardown EXIT

# Standalone teardown for kept runs: reuse the provisioner, so the
# success line below only prints when the kills and link deletions
# actually succeeded (the provisioner fails loudly on survivors).
if [[ "$TEARDOWN" == 1 ]]; then
  echo "==> tearing down kept run"
  provision_teardown_kept
  echo "torn down: daemons, taps, and $PROV_BRIDGE removed ($STATE_DIR retained)"
  exit 0
fi

# Reap strangers first: kept runs, killed ssh sessions, and
# supervisor restart loops all leave daemons holding our sockets.
# Idempotent: a no-op when nothing is running. Placed after the
# definitions above; bash reads top-down.
provision_kill_stale_daemons

echo "==> state dir $STATE_DIR"
provision_prepare_state "$RUN" "$WORK" "$BUILD_LOGS" "$SSH_KEY" "$STATE_DIR" "$FRESH"

# Subject staging (plan step 4): the contract lives in
# nix/microvm/stage-subject.sh — bounded ingestion, hostile-content
# refusal, and the narHash pin, in that order so the hash binds the
# measured bytes. Staged here, evaluated by nix below; the override
# flags are built from the staged copy and its pin.
STAGED_SUBJECT=""
SUBJECT_NARHASH=""
SUBJECT_FLAGS=()
if [[ -n "$SUBJECT_DIR" ]]; then
  echo "==> staging subject tree (bounded ingestion)"
  STAGED_SUBJECT="$WORK/subject"
  subject_stage "$SUBJECT_DIR" "$STAGED_SUBJECT" || die "subject staging failed (see above)"
  SUBJECT_NARHASH="$(subject_pin "$STAGED_SUBJECT")" || die "subject pinning failed (see above)"
  echo "    subject: $SUBJECT_BYTES bytes, $SUBJECT_FILES files, narHash $SUBJECT_NARHASH"
  # The override flags the manifest's [subject-slots] section
  # promises (see nix/microvm/shape-manifest.sh): keep them in sync —
  # a flag removed here must be removed there (N9).
  subject_build_flags "$STAGED_SUBJECT" "$SUBJECT_NARHASH"
fi

echo "==> building wyrd + nak + 3 VM runners (parallel: wall time is the slowest, not the sum)"
# Fail fast with no orphans: the first failed wait kills the siblings
# before dying, so a dead build never burns CPU through teardown. The
# array is complete before any wait below runs; kill errors on reaped
# or finished pids are expected and ignored. Per-build logs keep each
# failure attributable to its own file.
wait_build() { # <pid> <what>
  wait "$1" || { kill "${BUILD_PIDS[@]}" 2>/dev/null || true; die "$2"; }
}
BUILD_START=$(date +%s)
# With a subject, the host wyrd build is skipped: the suite's WYRD_BIN
# comes from the guest closure below, which is the same
# content-addressed path both sides (stronger than building twice —
# identity by construction, not by hoping two builds agree).
if [[ -z "$SUBJECT_DIR" ]]; then
  setpriv --reuid "$BUILD_USER" --regid "$BUILD_GID" --init-groups \
    env HOME="$BUILD_HOME" USER="$BUILD_USER" LOGNAME="$BUILD_USER" \
    nix build "$ROOT#packages.x86_64-linux.wyrd" --no-link --print-out-paths --print-build-logs \
    >"$BUILD_LOGS/nix-build-wyrd.out" 2>"$BUILD_LOGS/nix-build-wyrd.log" 9>&- &
  BUILD_WYRD=$!
  BUILD_PIDS=("$BUILD_WYRD")
else
  BUILD_PIDS=()
fi
# Pinned nak for the opacity probe (see the flake comment): same
# no-link pattern as wyrd, so the suite never floats the registry.
setpriv --reuid "$BUILD_USER" --regid "$BUILD_GID" --init-groups \
  env HOME="$BUILD_HOME" USER="$BUILD_USER" LOGNAME="$BUILD_USER" \
  nix build "$ROOT#packages.x86_64-linux.nak" --no-link --print-out-paths --print-build-logs \
  >"$BUILD_LOGS/nix-build-nak.out" 2>"$BUILD_LOGS/nix-build-nak.log" 9>&- &
BUILD_NAK=$!
BUILD_PIDS+=("$BUILD_NAK")
declare -A BUILD_PID=()
for role in peer-o peer-n relay; do
  setpriv --reuid "$BUILD_USER" --regid "$BUILD_GID" --init-groups \
    env HOME="$BUILD_HOME" USER="$BUILD_USER" LOGNAME="$BUILD_USER" \
    nix build "$ROOT#nixosConfigurations.wyrd-$role.config.microvm.runner.qemu" \
    "${SUBJECT_FLAGS[@]}" \
    --no-link --print-out-paths --print-build-logs \
    >"$BUILD_LOGS/nix-build-$role.out" 2>"$BUILD_LOGS/nix-build-$role.log" 9>&- &
  BUILD_PID[$role]=$!
  BUILD_PIDS+=("${BUILD_PID[$role]}")
done
if [[ -z "$SUBJECT_DIR" ]]; then
  wait_build "$BUILD_WYRD" "host build failed (see build-logs/nix-build-wyrd.log)"
fi
wait_build "$BUILD_NAK" "host nak build failed (see build-logs/nix-build-nak.log)"
for role in peer-o peer-n relay; do
  wait_build "${BUILD_PID[$role]}" "$role runner build failed (see build-logs/nix-build-$role.log)"
done
BUILD_S=$(( $(date +%s) - BUILD_START ))
echo "    builds took ${BUILD_S}s (parallel wall time)"
if [[ -z "$SUBJECT_DIR" ]]; then
  WYRD_OUT="$(tail -n 1 "$BUILD_LOGS/nix-build-wyrd.out")"
else
  # The subject's wyrd, read out of the guest closure it already
  # rode in on: the host store realized it while building the
  # runner, so this is a lookup, not a sixth build — and the path
  # is identical to what the guests execute, by construction. The
  # -wyrd-[0-9] match with the -wyrd-deps exclusion picks the built
  # binary package, not the vendored-sources package (which shares
  # the version suffix) nor any future -wyrd-N package.
  WYRD_OUT="$(setpriv --reuid "$BUILD_USER" --regid "$BUILD_GID" --init-groups \
    env HOME="$BUILD_HOME" USER="$BUILD_USER" LOGNAME="$BUILD_USER" \
    nix path-info -r "$(tail -n 1 "$BUILD_LOGS/nix-build-peer-o.out")" \
    | grep -E -- '-wyrd-[0-9]' | grep -v -- '-wyrd-deps' | head -n 1)"
  [[ -n "$WYRD_OUT" ]] || die "subject guest closure carries no wyrd package (see build-logs/nix-build-peer-o.log)"
fi
WYRD_BIN="$WYRD_OUT/bin/wyrd"
[[ -x "$WYRD_BIN" ]] || die "no wyrd binary at $WYRD_BIN"
echo "    $WYRD_BIN"
NAK_OUT="$(tail -n 1 "$BUILD_LOGS/nix-build-nak.out")"
NAK_BIN="$NAK_OUT/bin/nak"
[[ -x "$NAK_BIN" ]] || die "host build produced no nak binary (see build-logs/nix-build-nak.log)"
echo "    $NAK_BIN"
declare -A RUNNER_OUT=()
for role in peer-o peer-n relay; do
  RUNNER_OUT[$role]="$(tail -n 1 "$BUILD_LOGS/nix-build-$role.out")"
  [[ -x "${RUNNER_OUT[$role]}/bin/virtiofsd-run" ]] || die "$role runner build produced no virtiofsd-run (see build-logs/nix-build-$role.log)"
done
# Kept runs outlive the build: pin the closures as store GC roots so
# a later collection cannot invalidate a debugging session. Non-kept
# runs skip this — their state is torn down with the run and nothing
# may outlive it into the shared store.
if [[ "$KEEP" == 1 ]]; then
  for role in peer-o peer-n relay; do
    nix-store --add-root "$STATE_DIR/gc-runner-$role" -r "${RUNNER_OUT[$role]}" >/dev/null
  done
fi

echo "==> network: $PROV_BRIDGE ${PROV_NET}.0/24"
provision_network

echo "==> ssh key + guest env"
# Key, authorized_keys and run-tree ownership are provisioned with
# the state above (provision_prepare_state); the paths stay here
# because the suite environment below consumes them.
{
  echo "# Generated by run-microvm.sh; sourced by the suite in guests."
  printf 'WYRD_BIN=%q\n' "$WYRD_BIN"
  printf 'RELAY_PORT=%q\n' "$PROV_RELAY_PORT"
  printf 'RELAY_HOST=%q\n' "$PROV_NET.${PROV_IP[relay]}"
  printf 'RELAY_MANAGED=%q\n' "0"
  printf 'CHECKOUT=%q\n' "/etc/wyrd-tests"
  printf 'E2E_ROOT=%q\n' "/mnt/wyrd-state/run"
  printf 'WYRD_E2E_TOKEN=%q\n' "$E2E_TOKEN"
  # Owner pid for the guard's liveness tie: the orchestrator shares
  # this pid namespace, so kill -0 proves the minting wrapper is the
  # live lock holder (guests must never check this — their pids are
  # a different namespace).
  printf 'WYRD_E2E_PID=%q\n' "$$"
  printf 'E2E_RUST_LOG=%q\n' "${E2E_RUST_LOG:-}"
  printf 'E2E_INTERVAL=%q\n' "${E2E_INTERVAL:-}"
} > "$STATE_DIR/e2e-env.sh"

echo "==> booting guests"
BOOT_START=$(date +%s)
for role in peer-o peer-n relay; do
  provision_boot_role "$role" "${RUNNER_OUT[$role]}" "$WORK"
done
BOOT_S=$(( $(date +%s) - BOOT_START ))
echo "    boot took ${BOOT_S}s (virtiofsd + qemu per guest)"

echo "==> waiting for ssh"
SSH_START=$(date +%s)
provision_wait_ssh "$SSH_KEY"
SSH_S=$(( $(date +%s) - SSH_START ))
echo "    ssh wait took ${SSH_S}s"

# Relay readiness before the suite: the first relay dial happens
# deep in the legs, where a slow relay boot surfaces as an opaque
# convergence timeout. Fail fast with the real cause instead.
echo "==> waiting for relay"
RELAY_START=$(date +%s)
provision_wait_relay
RELAY_S=$(( $(date +%s) - RELAY_START ))
echo "    relay control plane reachable (wait took ${RELAY_S}s)"
echo "HOST sections: build=${BUILD_S}s boot=${BOOT_S}s ssh_wait=${SSH_S}s relay_wait=${RELAY_S}s"

# Host availability witness for relay-flap forensics. The 2026-10-08
# ~30s relay outages had no guest logs (teardown destroys the relay
# VM; fixed by the relay journal timer in relay.nix) and no host
# load record (the prime suspect is CI contention on the host), so
# neither layer could be classified afterwards. This background loop
# logs one line per 5s — timestamp, host loadavg, relay TCP state —
# into the run logs. Per-tick open-append-close: the phase-1 guest
# pre-clean unlinks $RUN/logs once per run, and a held fd would keep
# writing the unlinked inode invisibly past the wipe. 9>&-: the loop
# must never inherit the run lock fd — an orphaned logger would hold
# the flock after the runner dies and wedge every later run plus
# --teardown (same class as the daemon fd rule at boot).
AVAIL_LOG="$RUN/logs/host-availability.log"
availability_loop() {
  while true; do
    # Liveness without a pid watch: the runner holds the run lock
    # for the whole run, so if this logger can acquire it the runner
    # is gone on an untrapped signal — exit instead of appending to
    # a dead run's file forever (and double-writing beside the next
    # run's logger). Append-mode open: never truncate the lock file.
    if exec 8>>"$LOCK" && flock -n 8; then exit 0; fi
    exec 8>&-
    ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    # Comma-joined triple: a space-joined loadavg leaves the line
    # with no stable field boundaries for the machine-classified
    # timeline this witness exists for.
    load="$(cut -d' ' -f1-3 /proc/loadavg | tr ' ' ',')"
    if timeout 2 bash -c "</dev/tcp/$PROV_NET.${PROV_IP[relay]}/$PROV_RELAY_PORT" 2>/dev/null; then relay=ok; else relay=FAIL; fi
    printf '%s load=%s relay=%s\n' "$ts" "$load" "$relay" >> "$AVAIL_LOG"
    sleep 5
  done
}
# Stdout/stderr join the log file too: the loop must hold no pipe
# the runner's readers wait on past an abnormal exit (same class as
# the AGENTS.md pipe rule). 9>&- as before: an orphaned logger must
# never inherit the run lock fd.
availability_loop >>"$AVAIL_LOG" 2>&1 9>&- &
LOGGER_PID=$!
echo "    host availability witness logging to logs/host-availability.log (pid $LOGGER_PID)"

echo "==> running the suite"
# Suite capabilities for the dev loop: the suite names no privileged
# program (see tests/alpha-microvm.sh), so the dev loop — which runs
# the suite as root on its own machine — provides the three
# dev-only helpers under nix/microvm/dev-*.sh. On Odin the host
# provides host-owned equivalents (or leaves the unset ones at
# their task-path defaults); the dev helpers never execute there
# (pinned by tests/test-no-host-privilege.sh).
set +e
timeout 7200 env \
  "STATE_DIR=$STATE_DIR" \
  "SSH_KEY=$SSH_KEY" \
  "MICROVM_ONLY_STEP=$ONLY_STEP" \
  "WYRD_BIN=$WYRD_BIN" \
  "NAK_BIN=$NAK_BIN" \
  "WYRD_E2E_TOKEN=$E2E_TOKEN" \
  "WYRD_E2E_LOCK=$LOCK" \
  "WYRD_AS_GUEST_BIN=$ROOT/nix/microvm/dev-as-guest.sh" \
  "WYRD_PARTITION_BIN=$ROOT/nix/microvm/dev-partition.sh" \
  "WYRD_HAND_TO_GUEST_BIN=$ROOT/nix/microvm/dev-hand-to-guest.sh" \
  "E2E_INTERVAL=${E2E_INTERVAL:-}" \
  "RELAY_URL=ws://$PROV_NET.${PROV_IP[relay]}:$PROV_RELAY_PORT" \
  "PEER_O=e2e@$PROV_NET.${PROV_IP[peer-o]}" \
  "PEER_N=e2e@$PROV_NET.${PROV_IP[peer-n]}" \
  bash "$ROOT/tests/alpha-microvm.sh" 9>&-
STATUS=$?
set -e
# Stop the witness with the suite: teardown() (EXIT trap) covers the
# failure path, but a green run must not log through the report and
# teardown into the file. Cleared so teardown's kill is a no-op.
kill "$LOGGER_PID" 2>/dev/null || true
LOGGER_PID=""
[[ "$STATUS" == 0 ]] || die "microvm suite failed (exit $STATUS, logs under $RUN/logs)"
echo "HOST sections: build=${BUILD_S}s boot=${BOOT_S}s ssh_wait=${SSH_S}s relay_wait=${RELAY_S}s"
echo "microvm: suite green, logs under $RUN/logs"
