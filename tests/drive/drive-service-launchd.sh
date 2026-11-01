#!/usr/bin/env bash
# tests/drive/drive-service-launchd.sh — system-level evidence for the
# declarative drive service on macOS: the launchd daemon the module
# generates, not a hand-written mirror. Evaluates the darwin fixture,
# copies each evaluated serviceConfig verbatim and rewrites only the
# per-run fields (label, ProgramArguments paths, log paths) — a
# module restart-policy change shows up in what runs here. Lints both
# plists, bootstraps the vault in the user domain, and asserts the
# supervisor contract live: running with posture after load, back with
# a new pid after SIGKILL of the supervised child, and exactly one
# exit-78 with no retry against a held drive (stop-first ordering so
# the holder is the only possible contender; attempt verified by
# LastExitStatus and unchanged run count across the throttle window).
# The mount daemon is generated and linted the same way (its run
# needs macFUSE, so only the vault boots here).
#
# No sudo: the user agent domain carries identical KeepAlive semantics
# for what this leg proves (plist shape, wrapper contract, restart
# behavior). System-domain boot stays covered by module evaluation
# plus the NixOS guest leg. Run from the checkout:
#   ./tests/drive/drive-service-launchd.sh [--keep]
set -euo pipefail

KEEP=0
[[ "${1:-}" == "--keep" ]] || [[ $# == "0" ]] || { echo "usage: drive-service-launchd.sh [--keep]" >&2; exit 2; }
[[ "${1:-}" == "--keep" ]] && KEEP=1

command -v nix >/dev/null || { echo "FAIL: nix is required" >&2; exit 3; }
command -v python3 >/dev/null || { echo "FAIL: python3 is required" >&2; exit 3; }
command -v launchctl >/dev/null || { echo "FAIL: launchctl is required" >&2; exit 3; }
command -v plutil >/dev/null || { echo "FAIL: plutil is required" >&2; exit 3; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
REV="$(git -C "$ROOT" rev-parse HEAD)"
SHORT="$(git -C "$ROOT" rev-parse --short HEAD)"
LOG="${TMPDIR:-/tmp}/wyrd-drive-launchd-$SHORT.log"
: > "$LOG"
exec > >(tee -a "$LOG") 2>&1

say() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
die() { say "FAIL: $*"; exit 1; }

say "revision under test: $REV"
say "log: $LOG"

LABEL="md.enesis.wyrd.e2e-vault"
HOLDER_PID=""
SCRATCH="$ROOT/.drive-launchd-$SHORT"
mkdir -p "$SCRATCH"
cleanup() {
  # The lock holder is a background child of this shell: reap it
  # here so a failure or interruption never leaves it holding the
  # (deleted) scratch drive past the run.
  if [[ -n "${HOLDER_PID:-}" ]]; then kill "$HOLDER_PID" 2>/dev/null || true; wait "$HOLDER_PID" 2>/dev/null || true; HOLDER_PID=""; fi
  say "cleanup: booting out $LABEL"
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  if [[ "$KEEP" == "0" ]]; then rm -rf "$SCRATCH"; else say "kept scratch dir: $SCRATCH"; fi
  say "cleanup done"
}
trap cleanup EXIT
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true

say "building the wyrd package for this system"
SYS="$(nix eval --impure --raw --expr 'builtins.currentSystem')"
WYRD_BIN="$(nix build --print-out-paths --no-link "$ROOT#packages.$SYS.wyrd" | tail -n 1)/bin/wyrd"
[[ -x "$WYRD_BIN" ]] || die "no wyrd binary built"
say "wyrd: $WYRD_BIN"
say "building the supervisor wrapper the module names"
nix build --print-out-paths --no-link "$ROOT#packages.$SYS.wyrd-supervise" > "$SCRATCH/supervise.out" \
  || die "wyrd-supervise package failed to build"

say "provisioning scratch drive and secrets (idempotent across --keep reruns)"
if [[ -f "$SCRATCH/demo/DRIVE" ]]; then
  say "scratch drive already initialized; reusing identity and drive"
else
  head -c32 /dev/urandom > "$SCRATCH/identity"; chmod 600 "$SCRATCH/identity"
  printf e2e-test-passphrase > "$SCRATCH/passphrase"; chmod 600 "$SCRATCH/passphrase"
  "$WYRD_BIN" init "$SCRATCH/demo" --identity-file "$SCRATCH/identity" --passphrase-file "$SCRATCH/passphrase" \
    || die "drive init failed"
fi

say "generating the daemon plist from module evaluation"
nix eval --json "$ROOT#darwinConfigurations.wyrd-drive-test.config.launchd.daemons" > "$SCRATCH/daemons.json" \
  || die "darwin daemon evaluation failed"
python3 - "$SCRATCH/daemons.json" "$SCRATCH" "$LABEL" "$SCRATCH/supervise.out" <<'EOF' || exit 1
import json, os, plistlib, sys
daemons_path, scratch, label = sys.argv[1], sys.argv[2], sys.argv[3]
daemons = json.load(open(daemons_path))
names = sorted(daemons)
print("evaluated daemons:", " ".join(names))
assert "md.enesis.wyrd.vault-demo" in daemons, "vault daemon missing from module output"
assert "md.enesis.wyrd.mount-demo" in daemons, "mount daemon missing from module output"
vault = daemons["md.enesis.wyrd.vault-demo"]["serviceConfig"]
argv = list(vault["ProgramArguments"])
# The wrapper is a store path the module emits but evaluation never
# realizes: launchd exec of a missing binary dies instantly with no
# output at all (empty daemon log, no pid — exactly the silent
# failure this looked like). The flake exposes the same build as
# packages.*.wyrd-supervise; build it here and refuse loudly unless
# it is byte-identical to what the module names (same builder, same
# source, same nixpkgs — a mismatch means the leg would test a
# different artifact than the module ships).
wrapper = argv[0]
assert wrapper.endswith("/bin/wyrd-supervise"), wrapper
built = open(sys.argv[4]).read().strip().splitlines()
assert len(built) == 1, built
candidate = os.path.join(os.path.realpath(built[0]), "bin", "wyrd-supervise")
assert os.path.realpath(wrapper) == candidate, (built, wrapper)
assert os.path.isfile(candidate) and os.access(candidate, os.X_OK), candidate
out = []
skip_next = False
for i, a in enumerate(argv):
    if skip_next:
        skip_next = False
        continue
    if a == "/var/lib/wyrd/demo":
        out.append(scratch + "/demo")
    elif a.startswith("--posture-path="):
        # --flag=value form: the bare-path rules below never match
        # it, so rewrite the drive-rooted value explicitly.
        k, v = a.split("=", 1)
        if v.startswith("/var/lib/wyrd/demo"):
            v = scratch + "/demo" + v[len("/var/lib/wyrd/demo"):]
        out.append(k + "=" + v)
    elif a.startswith("/var/lib/wyrd/demo/"):
        out.append(scratch + "/demo" + a[len("/var/lib/wyrd/demo"):])
    elif a in ("/etc/wyrd/identity", "/etc/wyrd/passphrase"):
        out.append(scratch + "/" + a.rsplit("/", 1)[-1])
    elif a.startswith("/var/log/"):
        out.append(scratch + "/" + a.rsplit("/", 1)[-1])
    elif a == "wss://relay.example":
        out.append("wss://127.0.0.1:9")
    else:
        out.append(a)
def svc(name, args, logname, template):
    # The whole evaluated serviceConfig, with only the per-run
    # fields replaced: a module restart-policy regression must show
    # up in what this leg runs, never be papered over by a
    # hand-mirrored dictionary. Null-valued keys have no plist
    # encoding and mean "unset" to launchd, so drop them rather
    # than failing the dump.
    def strip_none(v):
        if isinstance(v, dict):
            return {k: strip_none(x) for k, x in v.items() if x is not None}
        if isinstance(v, list):
            return [strip_none(x) for x in v]
        return v
    out = strip_none(dict(template))
    out["Label"] = name
    out["ProgramArguments"] = args
    out["StandardOutPath"] = scratch + "/" + logname
    out["StandardErrorPath"] = scratch + "/" + logname
    return out
with open(scratch + "/vault.plist", "wb") as f:
    plistlib.dump(svc(label, out, "vault.log", vault), f, fmt=plistlib.FMT_XML)
mount = daemons["md.enesis.wyrd.mount-demo"]["serviceConfig"]
mount_argv = list(mount["ProgramArguments"])
mount_out = []
for a in mount_argv:
    if a == "/var/lib/wyrd/laptop":
        mount_out.append(scratch + "/demo")
    elif a.startswith("/var/lib/wyrd/laptop/"):
        mount_out.append(scratch + "/demo" + a[len("/var/lib/wyrd/laptop"):])
    else:
        mount_out.append(a)
with open(scratch + "/mount.plist", "wb") as f:
    plistlib.dump(svc(label + "-mount", mount_out, "mount.log", mount), f, fmt=plistlib.FMT_XML)
print("plist written for:", label)
EOF
plutil -lint "$SCRATCH/vault.plist" || die "generated vault plist fails lint"
plutil -lint "$SCRATCH/mount.plist" || die "generated mount plist fails lint"
say "PASS: both daemon plists generate and lint"

pid_of() { launchctl list "$LABEL" 2>/dev/null | sed -n 's/.*"PID" = \([0-9][0-9]*\);.*/\1/p' | head -n 1; return 0; }

say "bootstrapping $LABEL in the user domain"
launchctl bootstrap "gui/$(id -u)" "$SCRATCH/vault.plist" || die "bootstrap failed"
sleep 5
PID1="$(pid_of)"
[[ "$PID1" =~ ^[0-9]+$ ]] || die "service not running after bootstrap (pid: ${PID1:-none})"
[[ -f "$SCRATCH/demo/vault.posture" ]] || die "no posture sidecar after start"
say "PASS: service running after load (pid $PID1) with posture"

say "assert: back with a new pid after SIGKILL of the child"
CHILD="$(pgrep -P "$PID1" wyrd | head -n 1 || true)"
[[ -n "$CHILD" ]] || die "no supervised wyrd child under wrapper pid $PID1"
kill -KILL "$CHILD"
for _ in $(seq 1 12); do sleep 5; PID2="$(pid_of)"; [[ "$PID2" =~ ^[0-9]+$ && "$PID2" != "$PID1" ]] && break; done
[[ "${PID2:-}" =~ ^[0-9]+$ && "$PID2" != "$PID1" ]] || die "no new pid after child SIGKILL (was $PID1, now ${PID2:-none})"
say "PASS: SIGKILL restarted the service (pid $PID1 -> $PID2)"

say "assert: held drive fails once with 78 and no retry"
# Stop first: the contention proof below is only airtight if the
# vault cannot be the contender.
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
sleep 2
PID_STOPPED="$(pid_of)"
[[ -z "$PID_STOPPED" ]] || die "vault still running after bootout (pid $PID_STOPPED)"
python3 -c 'import fcntl, time; f = open("'"$SCRATCH"'/demo/LOCK"); fcntl.flock(f, fcntl.LOCK_EX); time.sleep(120)' >/dev/null 2>&1 &
HOLDER_PID=$!
# Prove the holder before asserting anything about the service: a
# fork that died instantly would leave an unheld "held drive" and
# make the refusal assertions vacuous. The vault is stopped, so a
# failed second-party probe names the helper as the holder — no
# third party can contend. Poll the contention itself rather than
# asserting once: process-exists (kill -0) races fork-vs-first-flock,
# and only the acquired lock is load-bearing.
if ! kill -0 "$HOLDER_PID" 2>/dev/null; then HOLDER_PID=""; die "holder died immediately after fork"; fi
HELD=""
for _ in $(seq 1 12); do
  if ! python3 -c 'import fcntl; f = open("'"$SCRATCH"'/demo/LOCK"); fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)' 2>/dev/null; then HELD=1; break; fi
  sleep 2
done
if [[ -z "$HELD" ]]; then
  kill "$HOLDER_PID" 2>/dev/null || true; wait "$HOLDER_PID" 2>/dev/null || true; HOLDER_PID=""
  die "second-party non-blocking flock kept succeeding: nothing holds the drive"
fi
say "holder verified (exclusive lock held)"
launchctl bootstrap "gui/$(id -u)" "$SCRATCH/vault.plist" || die "bootstrap under held drive failed"
sleep 12
print_job() { launchctl print "gui/$(id -u)/$LABEL" 2>/dev/null || true; }
print_field() { print_job | sed -n "s/^$1 = \([0-9][0-9]*\);.*/\1/p" | head -n 1; }
PID3="$(pid_of)"
if [[ "$PID3" =~ ^[0-9]+$ ]]; then
  die "held-drive service is running (pid $PID3), expected dead with 78"
fi
E3="$(print_job | sed -n 's/.*last exit code = \([0-9][0-9]*\).*/\1/p' | head -n 1)"
[[ "$E3" == "78" ]] || die "held-drive exit is ${E3:-unknown}, expected 78"
R3="$(print_field "runs")"
sleep 35
PID4="$(pid_of)"
[[ -z "$PID4" || "$PID4" == "-" ]] || die "held drive retried (pid $PID4), expected no retry"
E4="$(print_job | sed -n 's/.*last exit code = \([0-9][0-9]*\).*/\1/p' | head -n 1)"
[[ "$E4" == "78" ]] || die "held-drive exit moved to ${E4:-unknown} across the throttle window, expected a single 78"
R4="$(print_field "runs")"
[[ "$R4" == "$R3" ]] || die "run count moved ${R3:-?}->$R4 across the throttle window: a retry happened"
say "PASS: held drive failed once with 78, no retry"
# Release the holder before the recovery leg: the trap is only the
# backstop for failure paths.
kill "$HOLDER_PID" 2>/dev/null || true
wait "$HOLDER_PID" 2>/dev/null || true
HOLDER_PID=""
# Boot out the failed instance first: bootstrapping over a loaded
# job is refused, and a restart inside the throttle window after the
# 78 exit can refuse too — poll past either, and fail loud with
# launchd's own error.
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
BOOT_OK=""
for _ in $(seq 1 12); do
  if launchctl bootstrap "gui/$(id -u)" "$SCRATCH/vault.plist" 2>"$SCRATCH/booterr.txt"; then BOOT_OK=1; break; fi
  sleep 5
done
[[ -n "$BOOT_OK" ]] || { cat "$SCRATCH/booterr.txt" >&2; die "re-bootstrap after holder release failed"; }
sleep 5
PID5="$(pid_of)"
[[ "$PID5" =~ ^[0-9]+$ ]] || die "service did not recover after the holder released"
say "PASS: service recovers once the holder releases (pid $PID5)"

say "drive-service-launchd: ALL CHECKS PASSED at $REV"
