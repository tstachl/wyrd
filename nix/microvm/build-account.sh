#!/usr/bin/env bash
# nix/microvm/build-account.sh — the account the microVM runner's nix
# builds run as, the uid it runs as, the group it runs with, and the
# home it caches in.
#
# The decision functions are pure: they resolve a name or refuse, with
# no side effects, so the runner and its stub
# (tests/test-runner-build-account.sh) decide the same way without the
# stub needing a host. resolve_build_identity ties them together and
# reads the host (stat, id) to produce the identity; it changes
# nothing but the four variables it sets.
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

# build_home <account>
#
# Prints the account's home directory, the nix client's cache. HOME
# via tilde expansion, not getent: the suite unit's PATH is minimal and
# getent is not on it (bash builtins always are). The expansion is
# syntax, not trust: a missing passwd entry leaves a literal `~name`,
# so the caller checks that the home exists — before the run lock and
# before the multi-minute build — and the host's suite configuration is
# what creates it.
#
# The resolution is its own function so the stub can model the host's
# passwd entry instead of needing a real account with a real home.
build_home() {
  local account="$1"

  eval echo "~$account"
}

# resolve_build_identity <root> <named-account>
#
# Resolves the identity the builds run as and sets four variables:
# BUILD_USER, BUILD_UID, BUILD_GROUP and BUILD_HOME. Reads the host
# (stat, id) and refuses by name — message on stderr, return 1 — when
# the identity cannot be resolved, would be root, or could not open
# the tree it is asked to build.
resolve_build_identity() { # <root> <named-account>
  local root="$1" named="$2"
  local tree_owner tree_group host_named uid

  tree_owner="$(stat -c %U "$root")"
  BUILD_USER="$(build_account_choice "$tree_owner" "$named")" || return 1

  uid="$(id -u "$BUILD_USER" 2>/dev/null)" || {
    echo "error: cannot resolve the uid of build account '$BUILD_USER' on this host; set E2E_BUILD_USER in the host's suite configuration to an account that exists on the host" >&2
    return 1
  }
  # BUILD_UID is read by the runner, not here.
  # shellcheck disable=SC2034
  BUILD_UID="$(build_uid_allowed "$uid")" || return 1

  host_named=0
  [[ -z "$named" ]] || host_named=1
  tree_group="$(stat -c %G "$root")"
  case "$(build_group_source "$tree_group" "$host_named")" in
    account)
      BUILD_GROUP="$(id -gn "$BUILD_USER" 2>/dev/null)" || {
        echo "error: cannot resolve the primary group of build account '$BUILD_USER' on this host; set E2E_BUILD_USER in the host's suite configuration to an account that exists on the host" >&2
        return 1
      }
      ;;
    tree)
      BUILD_GROUP="$tree_group"
      ;;
  esac

  BUILD_HOME="$(build_home "$BUILD_USER")"
  [[ -d "$BUILD_HOME" ]] || {
    echo "error: no home directory for build account '$BUILD_USER'; the host's suite configuration creates it" >&2
    return 1
  }

  # The account must be able to build the tree it is handed: nix
  # copies the tree, so the account must be able to list it and read
  # what is in it, and for a git checkout nix's own fetch of the
  # repository applies its ownership rules on top (the reason the
  # builds ran as the checkout's owner until the host named an
  # account). Probe the part that can be probed — that the account can
  # list the tree and read its entries — now, as that account, so a
  # host that hands over a tree the account cannot build fails here by
  # name instead of minutes into the build. The test runs in bash,
  # which is on the unit's PATH.
  # shellcheck disable=SC2016
  if ! setpriv --reuid "$BUILD_USER" --regid "$BUILD_GROUP" --init-groups \
    env HOME="$BUILD_HOME" bash -c 'shopt -s nullglob dotglob
      [[ -r "$1" && -x "$1" ]] || exit 1
      for entry in "$1"/*; do
        [[ -r "$entry" ]] || exit 1
        if [[ -d "$entry" ]] && ! [[ -x "$entry" ]]; then exit 1; fi
      done' bash "$root"; then
    echo "error: build account '$BUILD_USER' cannot build the source tree at $root; the host's suite configuration must hand that account a tree it can build" >&2
    return 1
  fi
  return 0
}
