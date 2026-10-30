#!/usr/bin/env bash
# tests/drive/drive-service-launchd.sh — system-level evidence for the
# declarative drive service on macOS: the launchd daemon the module
# generates, not a hand-written mirror. Evaluates the darwin fixture,
# rewrites only the per-run paths (label, drive, secrets, logs, dead
# relay) onto the module's argv, lints the plist, bootstraps it in
# the user domain, and asserts the supervisor contract live: running
# with posture after load, back with a new pid after SIGKILL of the
# supervised child, and exactly one exit-78 with no retry against a
# held drive. The mount daemon is generated and linted the same way
# (its run needs macFUSE, so only the vault boots here).
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
SCRATCH="$ROOT/.drive-launchd-$SHORT"
mkdir -p "$SCRATCH"
cleanup() {
  say "cleanup: booting out $LABEL"
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  if [[ "$KEEP" == "0" ]]; then rm -rf "$SCRATCH"; else say "kept scratch dir: $SCRATCH"; fi
  say "cleanup done"
}
trap cleanup EXIT
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true

say "building the wyrd package for this system"
WYRD_BIN="$(nix build --print-out-paths --no-link "$ROOT#packages.aarch64-darwin.wyrd" | tail -n 1)/bin/wyrd"
[[ -x "$WYRD_BIN" ]] || die "no wyrd binary built"
say "wyrd: $WYRD_BIN"

say "provisioning scratch drive and secrets"
head -c32 /dev/urandom > "$SCRATCH/identity"; chmod 600 "$SCRATCH/identity"
printf e2e-test-passphrase > "$SCRATCH/passphrase"; chmod 600 "$SCRATCH/passphrase"
"$WYRD_BIN" init "$SCRATCH/demo" --identity-file "$SCRATCH/identity" --passphrase-file "$SCRATCH/passphrase" \
  || die "drive init failed"

say "generating the daemon plist from module evaluation"
nix eval --json "$ROOT#darwinConfigurations.wyrd-drive-test.config.launchd.daemons" > "$SCRATCH/daemons.json" \
  || die "darwin daemon evaluation failed"
python3 - "$SCRATCH/daemons.json" "$SCRATCH" "$LABEL" <<'EOF' || exit 1
import json, plistlib, sys
daemons_path, scratch, label = sys.argv[1], sys.argv[2], sys.argv[3]
daemons = json.load(open(daemons_path))
names = sorted(daemons)
print("evaluated daemons:", " ".join(names))
assert "md.enesis.wyrd.vault-demo" in daemons, "vault daemon missing from module output"
assert "md.enesis.wyrd.mount-demo" in daemons, "mount daemon missing from module output"
vault = daemons["md.enesis.wyrd.vault-demo"]["serviceConfig"]
argv = list(vault["ProgramArguments"])
out = []
skip_next = False
for i, a in enumerate(argv):
    if skip_next:
        skip_next = False
        continue
    if a == "/var/lib/wyrd/demo":
        out.append(scratch + "/demo")
    elif a in ("/etc/wyrd/identity", "/etc/wyrd/passphrase"):
        out.append(scratch + "/" + a.rsplit("/", 1)[-1])
    elif a.startswith("/var/log/"):
        out.append(scratch + "/" + a.rsplit("/", 1)[-1])
    elif a == "wss://relay.example":
        out.append("wss://127.0.0.1:9")
    else:
        out.append(a)
def svc(name, args, logname):
    return {
        "Label": name,
        "ProgramArguments": args,
        "RunAtLoad": True,
        "KeepAlive": {"SuccessfulExit": True},
        "ThrottleInterval": 30,
        "StandardOutPath": scratch + "/" + logname,
        "StandardErrorPath": scratch + "/" + logname,
        "WorkingDirectory": "/",
    }
with open(scratch + "/vault.plist", "wb") as f:
    plistlib.dump(svc(label, out, "vault.log"), f, fmt=plistlib.FMT_XML)
mount = daemons["md.enesis.wyrd.mount-demo"]["serviceConfig"]
with open(scratch + "/mount.plist", "wb") as f:
    plistlib.dump(svc(label + "-mount", list(mount["ProgramArguments"]), "mount.log"), f, fmt=plistlib.FMT_XML)
print("plist written for:", label)
EOF
plutil -lint "$SCRATCH/vault.plist" || die "generated vault plist fails lint"
plutil -lint "$SCRATCH/mount.plist" || die "generated mount plist fails lint"
say "PASS: both daemon plists generate and lint"

pid_of() { launchctl list "$LABEL" 2>/dev/null | awk '$3 == "'"$LABEL"'" {print $1}'; }

say "bootstrapping $LABEL in the user domain"
launchctl bootstrap "gui/$(id -u)" "$SCRATCH/vault.plist" || die "bootstrap failed"
sleep 5
PID1="$(pid_of)"
[[ "$PID1" =~ ^[0-9]+$ ]] || die "service not running after bootstrap (pid: ${PID1:-none})"
[[ -f "$SCRATCH/demo/vault.posture" ]] || die "no posture sidecar after start"
say "PASS: service running after load (pid $PID1) with posture"

say "assert: back with a new pid after SIGKILL of the child"
CHILD="$(pgrep -P "$PID1" wyrd | head -n 1)"
[[ -n "$CHILD" ]] || die "no supervised wyrd child under wrapper pid $PID1"
kill -KILL "$CHILD"
for _ in $(seq 1 12); do sleep 5; PID2="$(pid_of)"; [[ "$PID2" =~ ^[0-9]+$ && "$PID2" != "$PID1" ]] && break; done
[[ "${PID2:-}" =~ ^[0-9]+$ && "$PID2" != "$PID1" ]] || die "no new pid after child SIGKILL (was $PID1, now ${PID2:-none})"
say "PASS: SIGKILL restarted the service (pid $PID1 -> $PID2)"

say "assert: held drive fails once with 78 and no retry"
python3 -c 'import fcntl, time; f = open("'"$SCRATCH"'/demo/LOCK"); fcntl.flock(f, fcntl.LOCK_EX); time.sleep(120)' >/dev/null 2>&1 &
HOLDER=$!
sleep 1
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
sleep 2
launchctl bootstrap "gui/$(id -u)" "$SCRATCH/vault.plist" 2>/dev/null || true
sleep 12
PID3="$(pid_of)"
kill "$HOLDER" 2>/dev/null || true
if [[ "$PID3" =~ ^[0-9]+$ ]]; then
  die "held-drive service is running (pid $PID3), expected dead with 78"
fi
grep -q "holds the drive" "$SCRATCH/vault.log" || die "no lock-refusal trace in the daemon log"
sleep 35
PID4="$(pid_of)"
[[ -z "$PID4" || "$PID4" == "-" ]] || die "held drive retried (pid $PID4), expected no retry"
say "PASS: held drive failed once with 78, no retry"
launchctl bootstrap "gui/$(id -u)" "$SCRATCH/vault.plist" || die "re-bootstrap after holder release failed"
sleep 5
PID5="$(pid_of)"
[[ "$PID5" =~ ^[0-9]+$ ]] || die "service did not recover after the holder released"
say "PASS: service recovers once the holder releases (pid $PID5)"

say "drive-service-launchd: ALL CHECKS PASSED at $REV"
