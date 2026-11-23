#!/usr/bin/env bash
# tests/harness/test-cli-vocabulary.sh — enumeration test for the CLI
# vocabulary settlement (the deliberate alpha break: `drive … init`,
# `owner … start/status`, `materialization …`). Executable sources
# must not invoke the removed top-level commands.
#
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-cli-vocabulary.sh`. (Wired into the CI
# shell-tests job alongside the other stubs.)
#
# Scope: everything that executes — tests/{lima,microvm,gate,drive}/,
# tests/harness/ itself, nix/drive/, packaging/. docs/, CHANGELOG.md,
# and apps/ are out of scope: release history and the old-to-new
# migration map legitimately name the removed spellings.
#
# Discipline: the bans below match comments too, so a comment that
# needs one of these words must be reworded, not excepted. Mode and
# role values (`mode = "vault"`, `role set vault`), sidecar names
# (`vault.posture`), service names (`wyrd-vault-demo`), and issue
# numbers are not invocations and stay.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

# Collect executable sources. Bats phases are included: `run`/`on_o`
# route their strings to real shells.
mapfile -t SOURCES < <(
  find "$ROOT/tests/lima" "$ROOT/tests/microvm" "$ROOT/tests/gate" \
    "$ROOT/tests/drive" "$ROOT/tests/harness" "$ROOT/nix/drive" \
    "$ROOT/packaging" \
    \( -name '*.sh' -o -name '*.bash' -o -name '*.bats' -o -name '*.nix' \) \
    -not -name 'test-cli-vocabulary.sh' \
  | sort
)
[[ "${#SOURCES[@]}" -gt 0 ]] || die "no executable sources found"

# 1. No removed top-level invocations: `wyrd init`, `wyrd vault`,
# `wyrd policy` (word-boundary on both sides, so service names,
# mode values, and posture files never match).
if grep -rnE 'wyrd[[:space:]]+(init|vault|policy)([[:space:]]|$|")' "${SOURCES[@]}"; then
  die "removed top-level wyrd command invoked (see above)"
fi
pass "no wyrd init/vault/policy invocations"

# 2. No removed subcommands through the e2e helpers: the word right
# after the creds/role slot is the subcommand (`with_creds <c> vault`
# is banned; `with_creds <c> drive <dir> init` is the nested action
# and stays). Quoted `"$WYRD_BIN"` counts: the drive-service entry
# routes the binary through a quoted sudo/exec wrapper.
if grep -rnE '(with_creds[[:space:]]+[^[:space:]]+[[:space:]]+|guest_wyrd[[:space:]]+[^[:space:]]+[[:space:]]+|["'"'"']?\$WYRD_BIN["'"'"']?[[:space:]]+)(init|vault|policy)([[:space:]]|$|")' "${SOURCES[@]}"; then
  die "removed subcommand through e2e helper (see above)"
fi
pass "no init/vault/policy through with_creds/guest_wyrd/WYRD_BIN"

# 3. The Nix drive module maps the vault mode to the owner argv:
# parent flags precede `start`; nothing emits the removed subcommand.
for nix in "$ROOT/nix/drive/nixos.nix" "$ROOT/nix/drive/darwin.nix"; do
  grep >/dev/null '"owner"' "$nix" || die "$nix: no owner argv"
  grep >/dev/null '"start"' "$nix" || die "$nix: no start action"
  rest=$(tail -n +"$(grep -n '"owner"' "$nix" | head -1 | cut -d: -f1)" "$nix")
  i=$(printf '%s\n' "$rest" | grep -n '"--identity-file"' | head -1 | cut -d: -f1)
  st=$(printf '%s\n' "$rest" | grep -n '"start"' | head -1 | cut -d: -f1)
  [[ -n "$i" && -n "$st" && "$i" -lt "$st" ]] \
    || die "$nix: owner argv out of order (want owner < flags < start)"
done
pass "nix drive modules emit owner … start in order"

# 4. The shared lima helper nests drive/owner actions under parent
# credentials (`wyrd drive <dir> --identity-file … init`).
grep >/dev/null 'drive|owner)' "$ROOT/tests/lima/alpha-common.sh" \
  || die "alpha-common.sh: with_creds has no drive/owner branch"
pass "with_creds nests drive/owner actions"

echo "cli-vocabulary: $PASS checks passed"
