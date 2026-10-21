#!/usr/bin/env bash
# nix/microvm/build-account.sh — the account the microVM runner's nix
# builds run as, and the group they run with. Pure policy: it resolves
# a name or refuses, with no side effects, so the runner and its stub
# (tests/test-runner-build-account.sh) decide the same way without the
# stub needing a host.
#
# The host names the account (E2E_BUILD_USER), because the tree the
# runner builds can be a host-owned snapshot: a snapshot the host owns
# is root-owned, so its owner is not a build account. The tree's owner
# remains the fallback for an ordinary checkout, and a build that would
# run as root is refused rather than performed — the runner exists to
# keep the build off the privileged account that runs it, and losing
# that de-escalation silently (the build still works, just as root) is
# the failure this guards.

# build_account_choice <tree-owner> <named-account>
#
# Prints the account the build runs as. On a refusal, prints the
# reason on stderr and returns 1: the host named root (a host that
# misconfigured the de-escalation), no account is named and the tree's
# owner is root (a host-owned snapshot with no build account
# configured), or the name is not an account name at all. Every
# refusal names the setting to change.
#
# A tree owner stat cannot resolve is refused here rather than at the
# home check later: stat prints the literal UNKNOWN for it (never a
# numeric id), such an owner has no passwd entry and so no home to
# build under, and the early refusal names the way out. A host that
# names an account makes the tree's owner irrelevant.
build_account_choice() {
  local tree_owner="$1" named="$2"

  if [[ -n "$named" ]]; then
    if [[ "$named" == "root" ]]; then
      echo "error: the host named root as the build account; set E2E_BUILD_USER in the host's suite configuration to a non-root build account" >&2
      return 1
    fi
    if ! [[ "$named" =~ ^[a-zA-Z_][a-zA-Z0-9_.-]*$ ]]; then
      echo "error: E2E_BUILD_USER is not an account name: '$named'; set E2E_BUILD_USER in the host's suite configuration to the name of a build account on the host" >&2
      return 1
    fi
    printf '%s\n' "$named"
    return 0
  fi

  if [[ "$tree_owner" == "root" ]]; then
    echo "error: refusing to build as root: the tree is owned by root and the host named no build account; set E2E_BUILD_USER in the host's suite configuration to a non-root build account" >&2
    return 1
  fi
  if [[ "$tree_owner" == "UNKNOWN" ]]; then
    echo "error: the tree's owner cannot be resolved on this host; set E2E_BUILD_USER in the host's suite configuration to a non-root build account" >&2
    return 1
  fi
  if ! [[ "$tree_owner" =~ ^[a-zA-Z_][a-zA-Z0-9_.-]*$ ]]; then
    echo "error: the tree's owner is not an account name: '$tree_owner'; the host can name a build account with E2E_BUILD_USER" >&2
    return 1
  fi
  printf '%s\n' "$tree_owner"
}

# build_uid_allowed <uid>
#
# Prints the uid the build runs as. On a refusal, prints the reason
# on stderr and returns 1: the uid is 0. The invariant this branch
# exists to protect is uid-based, not name-based — a host whose
# passwd carries a second name for uid 0 would pass a name check and
# still be a root build — so the resolved uid is what decides, and the
# name check stays only the fast path.
build_uid_allowed() {
  local uid="$1"

  if [[ "$uid" == "0" ]]; then
    echo "error: refusing to build as uid 0; set E2E_BUILD_USER in the host's suite configuration to a non-root build account" >&2
    return 1
  fi
  printf '%s\n' "$uid"
}

# build_group_source <tree-group> <host-named>
#
# Prints where the build's group comes from: "account" for the build
# account's own primary group, "tree" for the group the tree carries.
# <host-named> is 1 when the host named the account, 0 when it did
# not.
#
# A host-named account always takes its own primary group, whatever
# the tree carries: the tree's group belongs to the tree's owner, and a
# build that keeps the privileged account's group keeps half of the
# privilege the named account drops. Without a named account the
# tree's group is used, as before — except when stat cannot resolve
# it (it prints UNKNOWN), where the account's primary group is the
# only group there is.
build_group_source() {
  local tree_group="$1" host_named="$2"

  if [[ "$host_named" == "1" ]]; then
    printf 'account\n'
    return 0
  fi
  if [[ "$tree_group" == "UNKNOWN" ]]; then
    printf 'account\n'
    return 0
  fi
  printf 'tree\n'
}
