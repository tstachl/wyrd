# .ngit/nix/workflows/default.nix — CI commands for the native Nix path.
#
# One command group per workflow file (shell.yaml → `shell`, rust.yaml
# → `rust`, nix.yaml → `nix`), one command per job. Commands are
# writeShellApplications whose runtimeInputs carry every tool the job
# shells out to — the guest is a plain NixOS VM, so nothing may be
# assumed beyond the closure.
{ pkgs, crane, src }:
let
  # The exact channel pinned in rust-toolchain.toml, so CI compiles
  # with the same rustc as the devenv shell and the flake builds.
  # pkgs already carries the rust-overlay (see pkgsFor in flake.nix).
  toolchain =
    pkgs.rust-bin.fromRustupToolchainFile
      (src + "/rust-toolchain.toml");
  craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

  # Vendored third-party sources, resolved once per lockfile: every
  # rust command below builds and tests offline. The vendor derivation
  # itself fetches at build time; job runtime never touches the
  # network for dependencies (the audit job is the documented
  # exception — the advisory DB is a live feed, not a dependency).
  vendor = craneLib.vendorCargoDeps {
    src = craneLib.cleanCargoSource src;
  };

  # Shared prelude for the cargo jobs: scratch CARGO_HOME (primed
  # with the vendor source replacement) and scratch target dir, both
  # on writable temp space — the checkout may be read-only and the
  # store always is. Everything after this runs offline.
  cargoOffline = pkgs.writeShellScript "wyrd-cargo-offline.sh" ''
    export CARGO_HOME="$(mktemp -d)"
    export CARGO_TARGET_DIR="$(mktemp -d)"
    if [ -f "${vendor}/config.toml" ]; then
      cat "${vendor}/config.toml" >>"$CARGO_HOME/config.toml"
    else
      printf '[source.crates-io]\nreplace-with = "nix-sources"\n[source.nix-sources]\ndirectory = "%s"\n' \
        "${vendor}" >>"$CARGO_HOME/config.toml"
    fi
    export CARGO_NET_OFFLINE=true
  '';

  # One cargo invocation per command, mirroring the act rust job's
  # flags exactly (plus offline, via the prelude). Fail-fast like the
  # act steps (bash -e). coreutils and gnugrep ride every job:
  # writeShellScript carries no PATH of its own, so the cargoOffline
  # prelude's mktemp/cat (and rust.check's grep over the feature
  # graph) resolve from the sourcing application's closure.
  cargoCmd = name: extraInputs: script:
    pkgs.writeShellApplication {
      inherit name;
      runtimeInputs = [ toolchain pkgs.coreutils pkgs.gnugrep ] ++ extraInputs;
      text = ''
        set -euo pipefail
        # shellcheck disable=SC1091
        source ${cargoOffline}
        ${script}
      '';
    };
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

  # Phase 2: rust gates. Each job mirrors one act step (same flags,
  # plus offline). The production feature-graph gate rides with
  # `check`: both ask "is the normal graph clean", and splitting
  # them would double the cheapest compile in the phase.
  rust.check = cargoCmd "rust-check" [ ] ''
    cargo check --color never --offline
    # Production feature graph excludes insecure-fast-kdf: the normal
    # dependency graph (no dev/test edges) must never enable it.
    cargo tree -e normal,features --workspace --offline >"$CARGO_TARGET_DIR/features.log"
    if grep -q insecure-fast-kdf "$CARGO_TARGET_DIR/features.log"; then
      echo "error: the normal dependency graph enables insecure-fast-kdf" >&2
      grep -n insecure-fast-kdf "$CARGO_TARGET_DIR/features.log" >&2
      exit 1
    fi
  '';

  rust.clippy = cargoCmd "rust-clippy" [ ] ''
    cargo clippy --workspace --all-targets --offline -- -D warnings
  '';

  rust.tests = cargoCmd "rust-tests" [ pkgs.cargo-nextest ] ''
    cargo nextest run --color never --workspace
  '';

  rust.doctests = cargoCmd "rust-doctests" [ ] ''
    cargo test --color never --workspace --doc --offline
  '';

  # The one job allowed network: the advisory DB is a live feed, not
  # a dependency, so offline vendoring does not apply to it. Cargo
  # itself still resolves offline; only the audit/deny DB fetches
  # reach the network.
  rust.audit = cargoCmd "rust-audit" [
    pkgs.cargo-audit
    pkgs.cargo-deny
  ] ''
    unset CARGO_NET_OFFLINE
    cargo audit --color never
    cargo deny check
  '';

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
