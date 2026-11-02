# .ngit/nix/workflows/default.nix — CI commands for the native Nix path.
#
# One command group per workflow file (shell.yaml → `shell`, rust.yaml
# → `rust`, nix.yaml → `nix`), one command per run job. Commands are
# writeShellApplications whose runtimeInputs carry every tool the job
# shells out to — the guest is a plain NixOS VM, so nothing may be
# assumed beyond the closure.
#
# The rust compile gates live one layer down as pure derivations
# (ngit-ci.builds, realised by rust.yaml's build jobs); only the
# audit job — which needs the network for the advisory DB — remains
# a run command here.
{ pkgs, crane, src }:
let
  # (No shared prelude: each command below is self-contained.)
in
{
  # Phase 1: hermetic harness suites. Deliberately no `nix`: the two
  # suites that can use it skip cleanly without it, and phase 1 stays
  # network-free (flake input fetches would need guest egress; the
  # nix evaluation phase covers that ground instead).
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

  # Phase 2: the audit job is the only remaining run command — the
  # advisory DB is a live feed, not a dependency, and sandbox-free
  # guest egress is the only way to fetch it. (The compile gates
  # moved to pure derivations under ngit-ci.builds, realised by
  # rust.yaml's build jobs.) git: cargo-audit shells out to the git
  # CLI to clone the advisory DB.
  rust.audit = pkgs.writeShellApplication {
    name = "rust-audit";
    runtimeInputs = with pkgs; [
      cargo-audit
      cargo-deny
      git
    ];
    text = ''
      set -euo pipefail
      cargo audit --color never
      cargo deny check
    '';
  };

  # Phase 3: nix evaluation and package builds. The two build jobs
  # realise the same derivations act built; the flake-check job is
  # the `nix flake check --no-build` equivalent (evaluation only, no
  # image assembly). Guest Nix required for the check job only; build
  # jobs never invoke nix in-guest (the provider realises them).
  nix.flake-check = pkgs.writeShellApplication {
    name = "nix-flake-check";
    # git: `nix flake check` shells out to git to fetch git+https
    # inputs (the gate input); the plain guest has no git without this.
    runtimeInputs = [ pkgs.nix pkgs.git ];
    text = ''
      set -euo pipefail
      # Materialize the nixpkgs tree behind pkgs.path before the
      # check: the image checks instantiate derivations through
      # gate's `import "''${pkgs.path}/nixos/lib/make-disk-image.nix"`,
      # and that import refuses a content-addressed tree copy whose
      # NAR is absent ("path ... is not valid") instead of
      # re-materializing it. Evaluating the path below re-materializes
      # the copy from the pristine flake input, so the check then
      # finds it valid. All four image roles share the one nixpkgs
      # input, hence the one copy; all four are named so a future
      # input split stays covered. Cold evaluation like the check
      # itself (--option eval-cache false): a cached path value must
      # never stand in for the copy.
      for role in peer-o peer-n relay drive-vm; do
        nix eval --raw --option eval-cache false \
          ".#nixosConfigurations.wyrd-$role.pkgs.path" > /dev/null
      done
      nix flake check --no-build --option eval-cache false
    '';
  };
}
