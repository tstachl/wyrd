#!/usr/bin/env bash
# tests/test-runner-build-account.sh — stub pinning the build-account
# policy of nix/microvm/run-microvm.sh: the host names the account the
# nix builds run as (E2E_BUILD_USER), the tree's owner is the
# fallback, and a build that would run as root is refused with a
# message naming the setting to change. Pure bash, no host, no nix:
# run it anywhere with `bash tests/test-runner-build-account.sh`.
# (Wired into CI alongside the other harness stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

POLICY="$ROOT/nix/microvm/build-account.sh"
[[ -f "$POLICY" ]] || die "build-account policy missing at $POLICY"
# shellcheck source=/dev/null
source "$POLICY"

# refuse <tree-owner> <named>: runs the policy, expects a refusal, and
# prints the refusal message. Fails (non-zero) when the policy
# allowed the choice.
refuse() { # <tree-owner> <named>
  if build_account_choice "$1" "$2" >"$TMP/out" 2>"$TMP/err"; then
    return 1
  fi
  cat "$TMP/err"
}

# The host names the account: it wins over the tree's owner, so a
# root-owned snapshot still builds as the named account.
out="$(build_account_choice root wyrd-build)" || die "named account refused"
[[ "$out" == "wyrd-build" ]] || die "named account ignored: got '$out'"
pass "the host's account wins over a root-owned tree"

# An ordinary checkout: the tree's owner builds as itself, unchanged.
out="$(build_account_choice thomas "")" || die "tree owner refused"
[[ "$out" == "thomas" ]] || die "tree owner not used: got '$out'"
pass "the tree's owner remains the fallback"

# A root-owned tree with no account named by the host: refused, not
# built as root. The refusal names the setting to change.
err="$(refuse root "")" || die "a root build was allowed"
[[ "$err" == *"root"* ]] || die "refusal does not say what it refuses: $err"
[[ "$err" == *"E2E_BUILD_USER"* ]] || die "refusal does not name the setting: $err"
pass "a root build with no named account is refused, naming the setting"

# A host that names root has misconfigured the de-escalation: the
# runner must refuse it, not honour it.
err="$(refuse thomas root)" || die "a host naming root was allowed"
[[ "$err" == *"E2E_BUILD_USER"* ]] || die "refusal does not name the setting: $err"
pass "a host naming root is refused, naming the setting"

# A name that is not an account is refused: the runner expands the
# account into a home path, so the name must never carry shell
# metacharacters. The literal is deliberate: it is what a compromised
# host configuration would try to inject.
# shellcheck disable=SC2016
err="$(refuse thomas '$(id)')" || die "a malformed account name was allowed"
[[ "$err" == *"E2E_BUILD_USER"* ]] || die "refusal does not name the setting: $err"
pass "a malformed account name is refused, naming the setting"

# The runner takes the account from the host's variable and asks the
# policy for it: the tree's owner alone must never decide.
RUNNER="$ROOT/nix/microvm/run-microvm.sh"
[[ -f "$RUNNER" ]] || die "runner missing at $RUNNER"
grep -q 'E2E_BUILD_USER' "$RUNNER" || die "runner does not read E2E_BUILD_USER"
grep -q 'build_account_choice' "$RUNNER" || die "runner does not use the policy"
pass "the runner takes the account from the host's E2E_BUILD_USER"

# --teardown builds nothing, so it must not depend on the host naming a
# build account: a host that never named one still has to be able to
# stop its kept run. The derivation sits behind the teardown guard.
guard_line="$(grep -n 'TEARDOWN" == 0' "$RUNNER" | head -1 | cut -d: -f1)"
# The pattern quotes the runner's own variable literally.
# shellcheck disable=SC2016
choice_line="$(grep -n 'build_account_choice "\$TREE_OWNER"' "$RUNNER" | head -1 | cut -d: -f1)"
[[ -n "$guard_line" && -n "$choice_line" ]] || die "the runner does not guard the build account derivation"
(( choice_line > guard_line )) || die "the build account is derived before the teardown guard"
pass "the build account is derived only for a run that builds"

echo "runner-build-account: $PASS checks passed"
