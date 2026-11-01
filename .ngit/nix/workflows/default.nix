# .ngit/nix/workflows/default.nix — CI commands for the native Nix path.
#
# One command group per workflow file (shell.yaml → `shell`, ...),
# one command per job. Commands are writeShellApplications whose
# runtimeInputs carry every tool the suites shell out to — the guest
# is a plain NixOS VM, so nothing may be assumed beyond the closure.
# Deliberately no `nix`: the two suites that can use it skip cleanly
# without it, and phase 1 stays network-free (flake input fetches
# would need guest egress; the nix evaluation phase covers that
# ground instead).
{ pkgs }:
{
  shell.suites = pkgs.writeShellApplication {
    name = "shell-suites";
    runtimeInputs = with pkgs; [
      bash
      coreutils
      findutils
      gnugrep
      gnused
      gawk
      git
      jq
      python3
      bats
      shellcheck
      lsof
      util-linux
      gnutar
    ];
    # Same E2E harness stubs as the act shell-tests job (excluding
    # .ngit/scripts/tests/run-all.sh, which stays act-only in phase
    # 1), so a native green means the same thing as an act green.
    # Fail-fast like the act step (bash -e): the first red suite
    # stops the job and its tail names the failure.
    text = ''
      set -euo pipefail
      say() { printf '===== %s =====\n' "$1"; }
      say "e2e harness stubs (native)"
      bash tests/harness/test-step-grammar.sh
      bash tests/harness/test-baked-set.sh
      bash tests/harness/test-shape-manifest.sh
      bash tests/harness/test-supervise.sh
      bash tests/harness/test-drive-probe.sh
      bash tests/harness/test-wyrd-module.sh
      bash tests/harness/test-probe-mark.sh
      bash tests/harness/test-e2e-guard.sh
      bash tests/harness/test-bash-syntax.sh
      bash tests/harness/test-relay-forensics.sh
      bash tests/harness/test-rendezvous.sh
      bash tests/harness/test-timeout-discipline.sh
      bash tests/harness/test-image-manifest.sh
      bash tests/harness/test-gate-entry.sh
      bash -n tests/harness/e2e-helpers.bash
      bash tests/harness/test-no-host-privilege.sh
      say "native shell suites green"
    '';
  };
}
