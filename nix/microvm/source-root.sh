#!/usr/bin/env bash
# nix/microvm/source-root.sh — the tree the runner builds.
#
# Sourced, never executed. Pure over explicit arguments: no side
# effects at source time, results via stdout. Requires `die` from
# the sourcer (run-microvm.sh and tests/test-source-root.sh each
# define their own): refusals print `FAIL: ...` to stderr and return
# nonzero, so `$( )`-wrapped callers observe the refusal without
# dying.
#
# The host hands the runner its revision-bound source tree as
# GATE_SOURCE (set by the gate wrapper's launch from the staged
# real path it checked out at the requested revision). The runner
# builds from that tree, never from git discovery of a mutable
# checkout, so the built content stays bound to the authorized
# revision. A set-but-unusable GATE_SOURCE is refused, never
# worked around: falling back to git discovery would silently
# unbind the build. An unset GATE_SOURCE keeps git discovery for
# the dev loop (`sudo ./nix/microvm/run-microvm.sh` from a
# checkout), unchanged.

# resolve_source_root <gate-source-or-empty>
#
# Prints the tree the runner builds. Refuses when GATE_SOURCE is
# set but is not a buildable tree (not a directory, or no flake),
# and when GATE_SOURCE is unset and the cwd is not a git checkout.
resolve_source_root() { # <gate-source-or-empty>
  local gate_source="$1" root=""

  if [[ -n "$gate_source" ]]; then
    [[ -d "$gate_source" ]] \
      || die "GATE_SOURCE is not a directory: $gate_source (the host must hand the runner its revision-bound source tree)"
    [[ -f "$gate_source/flake.nix" ]] \
      || die "GATE_SOURCE has no flake: $gate_source (the host must hand the runner its revision-bound source tree)"
    printf '%s\n' "$gate_source"
    return 0
  fi

  root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
  [[ -n "$root" ]] \
    || die "run from a wyrd checkout, or from a gate-provided source (GATE_SOURCE is unset and this is not a git checkout)"
  printf '%s\n' "$root"
}
