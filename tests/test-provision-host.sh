#!/usr/bin/env bash
# tests/test-provision-host.sh — stub suite for
# nix/microvm/provision-host.sh (the privileged half of the e2e
# run). Pure bash, no host, no nix: run it anywhere with
# `bash tests/test-provision-host.sh`. It pins the privilege
# boundary the P0 runner issue requires: the provisioner takes only
# data (store paths, taps, state dirs), refuses anything that is
# not an identified closure before any privileged operation, and
# fails loudly on stale daemons and surviving links. External
# commands are stub scripts on PATH that record their argv, so a
# refusal that runs clean proves no privileged operation fired.
# (Wired into the CI shell-tests job alongside the other stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PASS=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
STUBS="$TMP/stubs"
CALLS="$TMP/calls.log"
mkdir -p "$STUBS"
: > "$CALLS"
export CALLS

# Stub external commands: record argv, then behave as directed by
# the environment. IP_SHOW_RC governs `ip link show` (1 = absent,
# the provisioned-clean state; 0 = a surviving link). PGREP_PIDS
# governs `pgrep -f` output (empty = no daemons).
IP_SHOW_RC=1
PGREP_PIDS=""
export IP_SHOW_RC PGREP_PIDS
stub() { # <name> <body...>: write the stub script
  local name="$1"; shift
  { printf '#!/usr/bin/env bash\n'; printf '%s\n' "$@"; } > "$STUBS/$name"
  chmod +x "$STUBS/$name"
}
stub ip 'echo "ip $*" >> "$CALLS"; if [[ "$1 $2" == "link show" ]]; then exit "$IP_SHOW_RC"; fi; exit 0'
stub ss 'echo "ss $*" >> "$CALLS"; exit 1'
stub ssh 'echo "ssh $*" >> "$CALLS"; exit 1'
stub ssh-keygen 'echo "ssh-keygen $*" >> "$CALLS"; exit 0'
stub chown 'echo "chown $*" >> "$CALLS"; exit 0'
stub pkill 'echo "pkill $*" >> "$CALLS"; exit 0'
stub pgrep 'echo "pgrep $*" >> "$CALLS"; if [[ -n "$PGREP_PIDS" ]]; then printf "%s\n" $PGREP_PIDS; exit 0; fi; exit 1'
stub ps 'echo "ps $*" >> "$CALLS"; exit 1'
stub sleep 'exit 0'
stub timeout 'echo "timeout $*" >> "$CALLS"; exit 1'
export PATH="$STUBS:$PATH"

LIB="$ROOT/nix/microvm/provision-host.sh"
[[ -f "$LIB" ]] || { echo "  FAIL: provision lib missing at $LIB" >&2; exit 1; }
# The sourcer provides die; the test's dies without killing the
# suite because every refusal below runs in a subshell (see refuse).
die() { echo "  FAIL: $1" >&2; exit 1; }
# shellcheck source=/dev/null
source "$LIB"

pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }

# refuse <desc> <expected-substring> <command...>: the command must
# fail inside its subshell, and stderr must name the reason.
refuse() {
  local desc="$1" want="$2"; shift 2
  local err
  if err="$("$@" 2>&1)"; then
    die "$desc ran clean (expected refusal containing: $want)"
  fi
  [[ "$err" == *"$want"* ]] || die "$desc refusal hid the reason (want: $want; got: $err)"
  pass "$desc"
}

# refuse_quiet <desc> <expected-substring> <command...>: like
# refuse, plus no stub may have fired (the refusal precedes every
# privileged operation).
refuse_quiet() {
  local desc="$1" want="$2"; shift 2
  : > "$CALLS"
  refuse "$desc" "$want" "$@"
  [[ ! -s "$CALLS" ]] || die "$desc fired privileged operations before refusing: $(cat "$CALLS")"
}

# --- non-store runners are refused before anything privileged ---

refuse_quiet "checkout-relative runner refused" "never the checkout" \
  provision_boot_role peer-o "result/bin/runner" "$TMP/work"
refuse_quiet "tmp-path runner refused" "never the checkout" \
  provision_boot_role peer-o "/tmp/evil-runner" "$TMP/work"
refuse_quiet "empty runner refused" "never the checkout" \
  provision_boot_role peer-o "" "$TMP/work"
refuse_quiet "unknown role refused" "unknown role" \
  provision_boot_role peer-x "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-runner" "$TMP/work"

# --- host network facts are sane and match the guest sources ---

[[ "$PROV_BRIDGE" =~ ^br-[a-z]+$ ]] || die "bridge '$PROV_BRIDGE' is not host-shaped"
[[ "$PROV_NET" =~ ^10\.[0-9]+\.[0-9]+$ ]] || die "subnet '$PROV_NET' is not a 10/8 subnet"
pass "bridge and subnet are host-shaped"
for _b in peer-o peer-n relay; do
  case "$_b" in relay) _n=relay.nix;; *) _n="$_b.nix";; esac
  _nix="$ROOT/nix/microvm/$_n"
  [[ "${PROV_TAP[$_b]}" == "$(sed -nE 's/^.*id = "(tap-[a-z]+)";.*/\1/p' "$_nix")" ]] \
    || die "provision tap for $_b disagrees with $_n"
  [[ "${PROV_IP[$_b]}" == "$(sed -nE 's/^.*address = "10\.0\.7\.([0-9]+)";.*/\1/p' "$_nix")" ]] \
    || die "provision ip for $_b disagrees with $_n"
done
unset _b _n _nix
pass "provision taps and ips match the guest declarations"
for _r in "${PROV_SSH_ROLES[@]}"; do
  [[ -n "${PROV_TAP[$_r]:-}" ]] || die "ssh role $_r is not a declared box"
done
unset _r
pass "ssh roles are a subset of the declared boxes"

# --- teardown: keep mode is a no-op, full mode tears down ---

: > "$CALLS"
provision_teardown 1 >/dev/null 2>&1 || die "keep-mode teardown failed"
[[ ! -s "$CALLS" ]] || die "keep-mode teardown touched the host: $(cat "$CALLS")"
pass "keep-mode teardown touches nothing"
: > "$CALLS"
provision_teardown 0 >/dev/null 2>&1 || die "full teardown failed"
for _t in tap-o tap-n tap-r br-wyrd; do
  grep -qF "ip link del $_t" "$CALLS" || die "full teardown skipped link del $_t"
done
unset _t
pass "full teardown removes taps and bridge"

# --- teardown verification: silent when clean, loud on survivors ---

IP_SHOW_RC=1; export IP_SHOW_RC
: > "$CALLS"
provision_teardown_kept >/dev/null 2>&1 || die "clean kept-teardown failed"
pass "kept teardown passes when links are gone"
IP_SHOW_RC=0; export IP_SHOW_RC
refuse "surviving tap fails kept teardown" "survives teardown" provision_teardown_kept
IP_SHOW_RC=1; export IP_SHOW_RC

# --- stale daemons escalate and fail loudly ---

PGREP_PIDS="111 222"; export PGREP_PIDS
refuse "surviving daemons fail loudly" "survive SIGKILL" provision_kill_stale_daemons
PGREP_PIDS=""; export PGREP_PIDS
: > "$CALLS"
provision_kill_stale_daemons || die "clean daemon reap failed"
pass "daemon reap is a no-op when nothing runs"

# --- state prep: bounded, keyed, owned, no key minted twice ---

RUN="$TMP/run"; WORK="$TMP/work"; LOGS="$TMP/build-logs"
KEY="$TMP/sshkey"; STATEDIR="$TMP/state"
mkdir -p "$STATEDIR"
printf 'sentinel\n' > "$TMP/keepme"
printf 'key\n' > "$KEY"; printf 'key-pub\n' > "$KEY.pub"
mkdir -p "$RUN" && printf 'stale\n' > "$RUN/sentinel"
provision_prepare_state "$RUN" "$WORK" "$LOGS" "$KEY" "$STATEDIR" 1 \
  >/dev/null 2>&1 || die "state prep failed"
[[ ! -e "$RUN/sentinel" ]] || die "fresh prep kept the old run"
[[ -f "$STATEDIR/authorized_keys.pub" ]] || die "prep did not publish authorized_keys"
grep -qF "chown -R 1000:1000 $RUN" "$CALLS" || die "prep did not hand the run tree to uid 1000"
if grep -q "ssh-keygen" "$CALLS"; then die "prep minted a key that already existed"; fi
pass "state prep wipes, publishes and hands over without re-minting"

# --- waits fail fast with the cause, not a suite timeout ---

refuse "unreachable peer fails fast" "never accepted ssh" provision_wait_ssh "$KEY"
refuse "silent relay fails fast" "never listened" provision_wait_relay

# --- the boundary itself, pinned by grep ---

MOD="$LIB"
for _forbidden in 'git ' 'nix build' 'nix-store' 'setpriv' '$ROOT' 'alpha-microvm' 'timeout 7200' 'WYRD_BIN' 'E2E_TOKEN'; do
  grep -qF "$_forbidden" "$MOD" && die "provision module reaches past the boundary: $_forbidden"
done
unset _forbidden
pass "provision module reads no repo, builds nothing, runs no suite"
for _priv in 'ip tuntap' 'ip link del' 'ip addr add' 'virtiofsd-run >' 'microvm-run >' 'SOCK_PAT' 'TAP_DEV' 'TAP_IP'; do
  grep -qF "$_priv" "$ROOT/nix/microvm/run-microvm.sh" && die "runner still performs privileged op: $_priv"
done
unset _priv
pass "runner performs no privileged operation directly"

echo "provision-host: $PASS checks passed"
