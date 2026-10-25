#!/usr/bin/env bash
# tests/test-source-root.sh — stub suite for
# nix/microvm/source-root.sh (the tree the runner builds). Pure
# bash, no host, no nix: run it anywhere with
# `bash tests/test-source-root.sh`. It pins the binding rule: a
# gate-handed source is used as handed, a set-but-unusable one is
# refused (never worked around with git discovery, which would
# silently unbind the build), and an unset one keeps git discovery
# for the dev loop. (Wired into the CI shell-tests job alongside
# the other harness stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PASS=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
STUBS="$TMP/stubs"
mkdir -p "$STUBS"

# Fake git: prints $GIT_TOPLEVEL for rev-parse --show-toplevel, and
# fails otherwise (simulating outside-a-checkout).
GIT_TOPLEVEL=""
export GIT_TOPLEVEL
cat >"$STUBS/git" <<'STUB'
#!/usr/bin/env bash
if [[ "${1:-}" == "rev-parse" && -n "$GIT_TOPLEVEL" ]]; then
  printf '%s\n' "$GIT_TOPLEVEL"
  exit 0
fi
exit 1
STUB
chmod +x "$STUBS/git"
export PATH="$STUBS:$PATH"

LIB="$ROOT/nix/microvm/source-root.sh"
[[ -f "$LIB" ]] || { echo "  FAIL: source-root lib missing at $LIB" >&2; exit 1; }
# shellcheck source=/dev/null
source "$LIB"

pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

# refuse <desc> <expected-substring> <args...>: resolve_source_root
# must fail inside its subshell, and stderr must name the reason.
refuse() {
  local desc="$1" want="$2"
  shift 2
  local err
  if err="$(resolve_source_root "$@" 2>&1)"; then
    die "$desc ran clean (expected refusal containing: $want)"
  fi
  [[ "$err" == *"$want"* ]] || die "$desc refusal hid the reason (want: $want; got: $err)"
  pass "$desc"
}

# accept <desc> <want-output> <args...>: prints exactly want-output.
accept() {
  local desc="$1" want="$2"
  shift 2
  local out
  out="$(resolve_source_root "$@" 2>&1)" || die "$desc failed cleanly (expected output: $want)"
  [[ "$out" == "$want" ]] || die "$desc printed '$out', want '$want'"
  pass "$desc"
}

# --- a gate-handed source is used as handed ---
GOOD="$TMP/staged"
mkdir -p "$GOOD"
printf '{ }\n' > "$GOOD/flake.nix"
accept "valid GATE_SOURCE is used as handed" "$GOOD" "$GOOD"

# --- a set-but-unusable source is refused, never worked around ---
refuse "missing GATE_SOURCE dir refused" "not a directory" "$TMP/no-such-dir"
BARE="$TMP/bare"
mkdir -p "$BARE"
refuse "GATE_SOURCE without a flake refused" "has no flake" "$BARE"
# Even when git discovery would succeed, a set GATE_SOURCE that is
# unusable must not fall back to it: the fallback would silently
# unbind the build from the authorized revision.
GIT_TOPLEVEL="$TMP/checkout"
export GIT_TOPLEVEL
mkdir -p "$GIT_TOPLEVEL"
refuse "no silent fallback when GATE_SOURCE is set but bad" "has no flake" "$BARE"

# --- an unset source keeps git discovery for the dev loop ---
accept "unset GATE_SOURCE resolves via git" "$GIT_TOPLEVEL" ""
GIT_TOPLEVEL=""
export GIT_TOPLEVEL
refuse "unset GATE_SOURCE outside a checkout refused" "run from a wyrd checkout" ""

echo "source-root: $PASS checks passed"
