#!/usr/bin/env bash
# tests/test-runner-fd.sh — stub pinning the run-lock fd discipline in
# nix/microvm/run-microvm.sh and nix/microvm/provision-host.sh:
# every background launch (a line ending in a lone `&`) must close
# fd 9 (`9>&-`), like the virtiofsd, qemu, build, and logger
# launches, or a SIGTERM'd build sibling holds the whole-run flock
# past a build failure and refuses the next run and --teardown.
# Pure bash + grep, no guest, no nix: run it anywhere with
# `bash tests/test-runner-fd.sh`. (Wired into CI shell-tests alongside
# the step-grammar stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

for _f in nix/microvm/run-microvm.sh nix/microvm/provision-host.sh; do
  FILE="$ROOT/$_f"
  [[ -f "$FILE" ]] || die "runner file missing at $FILE"
  # A trailing `&&` is not a launch; only a lone `&` backgrounds.
  LAUNCHES="$(grep -cE '[^&]&$' "$FILE" || true)"
  # Daemon spawns background mid-line (`( ... & ) 9>&-`): the `&`
  # closes over the subshell and the fd close trails the paren.
  SUBLAUNCHES="$(grep -cE '& \)' "$FILE" || true)"
  (( LAUNCHES + SUBLAUNCHES > 0 )) || die "no background launches found in $_f (parser broken)"
  pass "found $LAUNCHES EOL + $SUBLAUNCHES subshell launch sites in $_f"
  LEAKY="$(grep -E '[^&]&$' "$FILE" | grep -v '9>&-' || true)"
  [[ -z "$LEAKY" ]] || die "background launches without 9>&- in $_f: $LEAKY"
  LEAKY_SUB="$(grep -E '& \)' "$FILE" | grep -v '9>&-' || true)"
  [[ -z "$LEAKY_SUB" ]] || die "subshell launches without 9>&- in $_f: $LEAKY_SUB"
  pass "every background launch closes fd 9 in $_f"
done
unset _f FILE LAUNCHES SUBLAUNCHES LEAKY LEAKY_SUB

echo "runner-fd: $PASS checks passed"
