#!/usr/bin/env bash
# nix/microvm/build-account.sh — the account the microVM runner's nix
# builds run as. Pure policy: it resolves a name or refuses, with no
# side effects, so the runner and its stub (tests/test-runner-build-account.sh)
# decide the same way without the stub needing a host.
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
build_account_choice() {
  local tree_owner="$1" named="$2"

  if [[ -n "$named" ]]; then
    if [[ "$named" == "root" ]]; then
      echo "error: the host named root as the build account; set E2E_BUILD_USER in the host's suite configuration to a non-root build account" >&2
      return 1
    fi
    if ! [[ "$named" =~ ^[a-zA-Z_][a-zA-Z0-9_.-]*$ ]]; then
      echo "error: E2E_BUILD_USER is not an account name: '$named'; set E2E_BUILD_USER in the host's suite configuration to an account that exists on the host" >&2
      return 1
    fi
    printf '%s\n' "$named"
    return 0
  fi

  if [[ "$tree_owner" == "root" ]]; then
    echo "error: refusing to build as root: the tree is owned by root and the host named no build account; set E2E_BUILD_USER in the host's suite configuration to a non-root build account" >&2
    return 1
  fi
  if ! [[ "$tree_owner" =~ ^[a-zA-Z_][a-zA-Z0-9_.-]*$ ]]; then
    echo "error: the tree's owner is not an account name: '$tree_owner'; the build account cannot be resolved" >&2
    return 1
  fi
  printf '%s\n' "$tree_owner"
}
