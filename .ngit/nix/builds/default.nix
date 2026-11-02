# .ngit/nix/builds/default.nix — Rust gate derivations for the native Nix path.
#
# Compile-once shape: rust-deps (buildDepsOnly) compiles third-party
# and dev-dependencies a single time per Cargo.lock; each gate takes
# those artifacts and compiles only workspace crates. Exposed as flat
# rust-* entries under checks.x86_64-linux, which rust.yaml's build
# jobs realise (`nix flake check` requires every checks entry to be
# a derivation, so the gates cannot nest the way ngit-ci.commands
# nests per workflow file).
{ pkgs, crane, src }:
let
  # The exact channel pinned in rust-toolchain.toml, so CI compiles
  # with the same rustc as the devenv shell and the flake builds.
  toolchain =
    pkgs.rust-bin.fromRustupToolchainFile
      (src + "/rust-toolchain.toml");
  craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

  manifest = builtins.fromTOML (builtins.readFile (src + "/Cargo.toml"));
  version = manifest.workspace.package.version;

  # Full checkout as Cargo sees it: tests read fixture data and
  # include_str! docs (contracts fixtures, architecture.md) at
  # compile and run time, so the rs+toml-only cleanCargoSource the
  # package build uses would starve them. target/ and VCS state
  # never enter: flake sources exclude them.
  fullSrc = src;

  # Shared dependency build: dummy crates stand in for workspace
  # members, so only third-party sources compile. doCheck stays on
  # so dev-dependencies land in the artifacts as well.
  rustDeps = craneLib.buildDepsOnly {
    pname = "wyrd";
    inherit version;
    src = craneLib.cleanCargoSource src;
    strictDeps = true;
    nativeBuildInputs = [ pkgs.pkg-config ];
    buildInputs = [ pkgs.fuse ];
    # Same dev profile as the gates: without this the hook default
    # (release) builds release artifacts nobody consumes, and every
    # gate recompiles the world in dev instead of sharing.
    env = { CARGO_PROFILE = ""; };
  };

  # Common gate inputs: shared artifacts plus the native prelude for
  # build scripts (fuser's pkg-config probe; libfuse2 link probe),
  # mirroring binFor's Linux inputs in flake.nix. Outputs are
  # unconsumed — a succeeding build IS the gate — so no artifact
  # install.
  #
  # Dev profile, matching the act steps (`cargo check`, plain
  # `cargo clippy/test/nextest`): crane defaults to release
  # (CARGO_PROFILE=release when unset), and a profile mismatch
  # silently defeats artifact sharing — release-built deps are
  # useless to dev-profile gates, which recompile everything from
  # scratch. Empty (not unset) so the hook's `${VAR-release}`
  # default does not kick in; plain `cargo` never read the variable
  # anyway.
  gateArgs = {
    inherit version;
    cargoArtifacts = rustDeps;
    src = fullSrc;
    strictDeps = true;
    doInstallCargoArtifacts = false;
    nativeBuildInputs = [ pkgs.pkg-config ];
    buildInputs = [ pkgs.fuse ];
    env = { CARGO_PROFILE = ""; };
  };

  # check, plus the production feature-graph gate that rode with it
  # in the act steps: `cargo check` proves the workspace compiles;
  # `cargo tree` proves the normal graph stays free of
  # insecure-fast-kdf.
  rustCheck = craneLib.mkCargoDerivation (gateArgs // {
    pname = "wyrd";
    pnameSuffix = "-check";
    doCheck = false;
    buildPhaseCargoCommand = ''
      cargo check --color never
      cargo tree -e normal,features --workspace >./features.log
      if grep -q insecure-fast-kdf ./features.log; then
        echo "error: the normal dependency graph enables insecure-fast-kdf" >&2
        grep -n insecure-fast-kdf ./features.log >&2
        exit 1
      fi
    '';
  });

  # clippy through the pinned toolchain (not nixpkgs' clippy: under
  # -D warnings any version drift is fatal).
  rustClippy = craneLib.mkCargoDerivation (gateArgs // {
    pname = "wyrd";
    pnameSuffix = "-clippy";
    doCheck = false;
    buildPhaseCargoCommand =
      "cargo clippy --workspace --all-targets --color never -- -D warnings";
  });

  # Nextest default profile: the PR gate.
  rustTests = craneLib.cargoNextest (gateArgs // {
    pname = "wyrd";
    cargoNextestExtraArgs = "--color never --workspace";
  });

  # Nextest slow profile: the master-CI supplement (live-relay
  # integration, ceiling-scale admission, measured heavyweights
  # excluded from the default PR gate so PR runs stay fast).
  # Same compile-once shape: shares rust-deps, compiles only
  # workspace crates. Public-relay and kernel-FUSE tests stay
  # #[ignore]d and never run in CI, so this stays sandbox-safe.
  rustSlowTests = craneLib.cargoNextest (gateArgs // {
    pname = "wyrd";
    cargoNextestExtraArgs = "--color never --workspace --profile slow";
  });

  # Doctests run in the check phase.
  rustDoctests = craneLib.cargoDocTest (gateArgs // {
    pname = "wyrd";
    cargoTestExtraArgs = "--color never --workspace";
  });
in
{
  rust-deps = rustDeps;
  rust-check = rustCheck;
  rust-clippy = rustClippy;
  rust-tests = rustTests;
  rust-slow-tests = rustSlowTests;
  rust-doctests = rustDoctests;
}
