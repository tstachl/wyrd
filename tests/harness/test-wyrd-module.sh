#!/usr/bin/env bash
# tests/harness/test-wyrd-module.sh — the declarative drive module evaluates
# to the supervisor contract on both platforms: the wrapper with the
# per-supervisor restart status, the terminal lock code excluded from
# restarts, the switch-time conflict probe, and the posture path.
# Pure evaluation only (`nix eval`, no builds, no guests, no
# supervisor), so it runs anywhere nix runs. Fetches whole config
# objects as `--json` and asserts in python3: exact matches, never
# substrings of command lines.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

command -v nix >/dev/null || { echo "SKIP: nix is required"; exit 0; }
command -v python3 >/dev/null || { echo "SKIP: python3 is required"; exit 0; }

# Client/daemon version skew on a dev machine can surface a
# single-line phantom (`dynamic-derivations ... disabled`) for an
# evaluation that is otherwise sound. Retry exactly that line, up to
# three attempts; any other failure fails fast so a real regression
# never hides behind a retry.
nix_eval() {
    attempt=1
    while [ "$attempt" -le 3 ]; do
        if out=$(nix eval --json "$@" 2>&1); then
            printf '%s' "$out"
            return 0
        fi
        case "$out" in
        "error: experimental Nix feature 'dynamic-derivations' is disabled"*)
            attempt=$((attempt + 1))
            sleep 2
            ;;
        *)
            printf '%s' "$out" >&2
            return 1
            ;;
        esac
    done
    printf '%s' "$out" >&2
    return 1
}

cd "$ROOT"
NIXOS='.#nixosConfigurations.wyrd-drive-test'
DARWIN='.#darwinConfigurations.wyrd-drive-test'

check() {
    # check <label> <nix-attrpath> <python-predicate>.
    # The predicate receives the decoded JSON value as `v`.
    local label="$1" attr="$2" predicate="$3"
    if nix_eval "$attr" | python3 -c "
import json, sys
v = json.load(sys.stdin)
assert $predicate, '''$label'''
"; then
        pass "$label"
    else
        die "$label"
    fi
}

# --- NixOS: vault unit supervises the wrapper -----------------------
check "the vault unit supervises the wrapper with the systemd restart status" \
    "$NIXOS.config.systemd.services.wyrd-vault-demo.serviceConfig" \
    "'--restart-status=1' in v['ExecStart'] and ' vault /var/lib/wyrd/demo ' in v['ExecStart']"
check "the vault unit wires the posture path and the declared log" \
    "$NIXOS.config.systemd.services.wyrd-vault-demo.serviceConfig" \
    "'--posture-path=/var/lib/wyrd/demo/vault.posture' in v['ExecStart'] and '--log-file /var/log/wyrd-demo.log' in v['ExecStart']"

# --- NixOS: mount unit supervises the wrapper -----------------------
check "the mount unit supervises the wrapper with mount mode and mountpoint" \
    "$NIXOS.config.systemd.services.wyrd-mount-demo.serviceConfig" \
    "'--restart-status=1' in v['ExecStart'] and ' mount /var/lib/wyrd/laptop /mnt/wyrd' in v['ExecStart'] and '--posture-path=/var/lib/wyrd/laptop/mount.posture' in v['ExecStart']"

# --- NixOS: restart policy recovers, never retries the lock ---------
check "the unit restarts failures and never the terminal lock code" \
    "$NIXOS.config.systemd.services.wyrd-vault-demo.serviceConfig" \
    "v['Restart'] == 'on-failure' and v['RestartPreventExitStatus'] == [78]"
check "restarts are rate-limited" \
    "$NIXOS.config.systemd.services.wyrd-vault-demo" \
    "v['startLimitBurst'] == 3"

# --- NixOS: switch-time probe runs at activation --------------------
check "activation probes the holder with the unit PID as reference" \
    "$NIXOS.config.system.activationScripts.wyrdDrives" \
    "\"wyrd-drive-probe '/var/lib/wyrd/demo'\" in v['text'] and 'systemctl show -p MainPID' in v['text']"

# --- nix-darwin: daemon plist carries the wrapper contract ----------
check "the launchd daemon restarts on zero with the posture path wired" \
    "$DARWIN.config.launchd.daemons" \
    "'--restart-status=0' in ' '.join(v['md.enesis.wyrd.vault-demo']['serviceConfig']['ProgramArguments']) and '--posture-path=/var/lib/wyrd/demo/vault.posture' in ' '.join(v['md.enesis.wyrd.vault-demo']['serviceConfig']['ProgramArguments']) and v['md.enesis.wyrd.vault-demo']['serviceConfig']['KeepAlive'].get('SuccessfulExit') is True and v['md.enesis.wyrd.vault-demo']['serviceConfig']['ThrottleInterval'] == 30"
check "the darwin mount daemon wraps mount mode with its posture path" \
    "$DARWIN.config.launchd.daemons" \
    "' mount /var/lib/wyrd/laptop /Volumes/Wyrd' in ' '.join(v['md.enesis.wyrd.mount-demo']['serviceConfig']['ProgramArguments']) and '--posture-path=/var/lib/wyrd/laptop/mount.posture' in ' '.join(v['md.enesis.wyrd.mount-demo']['serviceConfig']['ProgramArguments'])"
check "darwin activation probes the holder with the daemon PID as reference" \
    "$DARWIN.config.system.activationScripts.wyrdDrives" \
    "\"wyrd-drive-probe '/var/lib/wyrd/demo'\" in v['text'] and 'launchctl list' in v['text']"

echo "wyrd-module: $PASS checks passed"
