#!/usr/bin/env bash
# tests/harness/test-rendezvous.sh — the cross-guest rendezvous channel
# works loopback: a signal published where a per-guest server serves
# it stays fetchable after the writer exits (the run-scoped property
# the suite depends on: legs never start or stop the server).
#
# Pure bash + python3 + loopback http: no nix, no guest, no Incus.
# Run it anywhere with `bash tests/harness/test-rendezvous.sh`.
# Skips when python3 or timeout is missing (macOS without coreutils).
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

command -v python3 >/dev/null 2>&1 || { echo "  SKIP: python3 missing"; exit 0; }
command -v timeout >/dev/null 2>&1 || { echo "  SKIP: timeout missing"; exit 0; }

FIXTURE="$(mktemp -d)"
SERVER_PID=""
trap 'rm -rf "$FIXTURE"; kill "$SERVER_PID" 2>/dev/null || true' EXIT
ENV_FILE="$FIXTURE/env.sh"
: >"$ENV_FILE"
PORT=18779

# Source the real peer_* functions in a subshell with a stub env, so
# the test exercises the shipped code, not a copy. The sourced
# library's EXIT trap reports timing on stdout; suppress it here —
# the verdict is the exit status, and failures speak on stderr.
peer_sh() { # <e2e-root> <peer-ip> <args...>
  local root="$1" peer="$2"; shift 2
  (
    set -u
    export E2E_ENV_FILE="$ENV_FILE" E2E_ROOT="$root" RENDEZVOUS_PORT="$PORT"
    # shellcheck source=/dev/null
    source "$ROOT/tests/lima/alpha-common.sh" >/dev/null 2>&1
    PEER_IP="$peer"
    "$@"
  ) >/dev/null
}

# Local signal: published and waited in one guest (the Lima shape).
peer_sh "$FIXTURE/a" "" peer_signal local-1
peer_sh "$FIXTURE/a" "" peer_wait 5 local-1 \
  || die "local signal not visible"
pass "local signal resolves without the network"

# Post-exit fetchability: the serving side mimics the boot service
# (stdlib http.server over the signals dir, started before the writer
# runs and surviving it), the writer signals and exits, and a second
# shell fetches as the peer would.
mkdir -p "$FIXTURE/a/signals" "$FIXTURE/b/signals"
# Bind loopback only: the fixture must not expose the host or
# collide with a developer service on all interfaces.
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$FIXTURE/a/signals" >/dev/null 2>&1 &
SERVER_PID=$!
sleep 1
kill -0 "$SERVER_PID" 2>/dev/null || die "loopback signal server did not start"
peer_sh "$FIXTURE/a" "" peer_signal remote-1
# Writer exits here: if the channel were leg-scoped, the server would
# die with it and the fetch below would burn its whole budget.
printf 'hello-peer' > "$FIXTURE/a/signals/remote-1"
peer_sh "$FIXTURE/b" "127.0.0.1" peer_wait 10 remote-1 \
  || die "peer signal not fetchable after writer exit"
[[ "$(cat "$FIXTURE/b/signals/remote-1")" == "hello-peer" ]] \
  || die "fetched signal body differs"
pass "peer signal stays fetchable after writer exit, byte-identical"

# Missing signal fails closed within budget (no hang).
start="$(date +%s)"
if peer_sh "$FIXTURE/b" "127.0.0.1" peer_wait 2 never-there; then
  die "missing signal reported found"
fi
elapsed=$(( $(date +%s) - start ))
(( elapsed < 15 )) || die "missing signal wait took ${elapsed}s"
pass "missing signal fails closed within budget"

kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""

echo "rendezvous: $PASS checks passed"
