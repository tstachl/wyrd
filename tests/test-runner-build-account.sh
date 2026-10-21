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

# refuse_uid <uid>: the same shape for the uid check.
refuse_uid() { # <uid>
  if build_uid_allowed "$1" >"$TMP/out" 2>"$TMP/err"; then
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
[[ "$err" == *"not an account name"* ]] || die "refusal does not say what it refuses: $err"
pass "a malformed account name is refused, naming the setting"

# An owner stat cannot resolve (it prints the literal UNKNOWN, never a
# numeric id) has no passwd entry and so no home to build under:
# refused, and the way out is named.
err="$(refuse UNKNOWN "")" || die "an unresolved tree owner was allowed"
[[ "$err" == *"E2E_BUILD_USER"* ]] || die "refusal does not name the setting: $err"
pass "an unresolved tree owner is refused, naming the setting"

# A host-named account makes the tree's owner irrelevant, unresolved
# owner and all: the build runs as the account the host named.
out="$(build_account_choice UNKNOWN wyrd-build)" || die "named account refused"
[[ "$out" == "wyrd-build" ]] || die "named account ignored: got '$out'"
pass "the host's account wins over an unresolved tree owner"

# The group: a host-named account runs with its own primary group,
# whatever the tree carries — including when the named account owns
# the tree, where the tree's group may be a foreign one.
[[ "$(build_group_source staff 1)" == "account" ]] || die "named account does not take its own group"
[[ "$(build_group_source root 1)" == "account" ]] || die "named account does not take its own group"
pass "a host-named account runs with its own primary group"

# Without a named account the tree's group is used, as before.
[[ "$(build_group_source staff 0)" == "tree" ]] || die "tree group not used for the fallback"
pass "the tree's group remains the fallback"

# A tree group stat cannot resolve falls back to the account's own.
[[ "$(build_group_source UNKNOWN 0)" == "account" ]] || die "unresolved tree group does not fall back"
pass "an unresolved tree group falls back to the account's own group"

# The root question is uid-based, not name-based: a second name for
# uid 0 would pass a name check and still be a root build.
err="$(refuse_uid 0)" || die "uid 0 was allowed"
[[ "$err" == *"E2E_BUILD_USER"* ]] || die "refusal does not name the setting: $err"
pass "uid 0 is refused, naming the setting"
[[ "$(build_uid_allowed 1000)" == "1000" ]] || die "a non-root uid was refused"
pass "a non-root uid is allowed"

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
# The greps carry `|| true`: under `set -e` a vanished pattern would
# abort the pipeline before the diagnosis below could print.
guard_line="$(grep -n 'TEARDOWN" == 0' "$RUNNER" | head -1 | cut -d: -f1 || true)"
# The pattern quotes the runner's own variable literally.
# shellcheck disable=SC2016
choice_line="$(grep -n 'build_account_choice "\$TREE_OWNER"' "$RUNNER" | head -1 | cut -d: -f1 || true)"
[[ -n "$guard_line" && -n "$choice_line" ]] || die "the runner does not guard the build account derivation"
(( choice_line > guard_line )) || die "the build account is derived before the teardown guard"
pass "the build account is derived only for a run that builds"

echo "runner-build-account: $PASS checks passed"
