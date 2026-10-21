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

# build_gid_allowed <gid>
#
# Prints the gid the build runs with. On a refusal, prints the reason
# on stderr and returns 1: the gid is 0. The named account's own
# primary group is the one the branch chooses, so it is held to the
# same rule as the uid: a build that keeps gid 0 keeps half of the
# privilege the account dropped.
build_gid_allowed() {
  local gid="$1"

  if [[ "$gid" == "0" ]]; then
    echo "error: refusing to build with gid 0; set E2E_BUILD_USER in the host's suite configuration to an account whose primary group is not the privileged one" >&2
    return 1
  fi
  printf '%s\n' "$gid"
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
  local tree_owner tree_group host_named uid gid probe_rc

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
      # The number, not the name: setpriv takes the number, and an
      # account whose primary gid has no entry in the host's group
      # database (LDAP, SSSD, a bare-gid account) still builds. The
      # name is for the log line, and its absence is not a refusal.
      gid="$(id -g "$BUILD_USER" 2>/dev/null)" || {
        echo "error: cannot resolve the primary group of build account '$BUILD_USER' on this host; set E2E_BUILD_USER in the host's suite configuration to an account that exists on the host" >&2
        return 1
      }
      gid="$(build_gid_allowed "$gid")" || return 1
      BUILD_GID="$gid"
      BUILD_GROUP="$(id -gn "$BUILD_USER" 2>/dev/null || printf '%s' "$gid")"
      ;;
    tree)
      # The tree's own group, as before: an ordinary checkout is
      # reached through the group it carries. The branch did not
      # choose this group, so it does not hold it to the gid rule the
      # named account's group is held to; the tree's group is the
      # tree's business, and an ordinary checkout's group is not the
      # privileged one.
      BUILD_GROUP="$tree_group"
      # BUILD_GID is read by the runner, not here.
      # shellcheck disable=SC2034
      BUILD_GID="$(stat -c %g "$root")"
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
  # list the tree, read its top-level entries, and write the home the
  # nix client caches in — now, as that account, so a host that hands
  # over a tree the account cannot build fails here by name instead of
  # minutes into the build. The probe reads the top level only: a
  # deeper problem (a file inside the tree the account cannot read)
  # surfaces in the build, where nix names it. That scope is the scope
  # of what gets copied: the runner builds a path flake
  # (`nix build "$ROOT#..."`), so nix copies the working tree, not the
  # repository's tracked files. The test runs in bash, which is on the
  # unit's PATH. A symlink is copied as a link, so its target is not
  # read; the exit code says which check failed (1 the tree, 2 the
  # home).
  probe_rc=0
  # shellcheck disable=SC2016
  setpriv --reuid "$BUILD_USER" --regid "$BUILD_GID" --init-groups \
    env HOME="$BUILD_HOME" bash -c 'shopt -s nullglob dotglob
      [[ -r "$1" && -x "$1" ]] || exit 1
      for entry in "$1"/*; do
        [[ -L "$entry" ]] && continue
        [[ -r "$entry" ]] || exit 1
        if [[ -d "$entry" ]] && ! [[ -x "$entry" ]]; then exit 1; fi
      done
      [[ -w "$HOME" ]] || exit 2' bash "$root" || probe_rc=$?
  if [[ "$probe_rc" != 0 ]]; then
    if [[ "$probe_rc" == 2 ]]; then
      echo "error: build account '$BUILD_USER' cannot write its home $BUILD_HOME; the host's suite configuration must create it for that account" >&2
    else
      echo "error: build account '$BUILD_USER' cannot read the source tree at $root; the host's suite configuration must hand that account a tree it can read" >&2
    fi
    return 1
  fi
  return 0
}
