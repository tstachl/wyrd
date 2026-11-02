# .ngit/nix/workflows/default.nix — CI commands for the native Nix path.
#
# One command group per workflow file (shell.yaml → `shell`, rust.yaml
# → `rust`, nix.yaml → `nix`, dist.yaml → `dist`, page.yaml → `page`),
# one command per run job. Commands are
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
  # The exact channel pinned in rust-toolchain.toml, so the audit job
  # dispatches `cargo audit` / `cargo deny` through the same cargo as
  # the flake builds. (The subcommand binaries alone are not enough:
  # cargo discovers them via the driver on PATH.)
  toolchain =
    pkgs.rust-bin.fromRustupToolchainFile
      (src + "/rust-toolchain.toml");
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
    # Same E2E harness stubs as the former act shell-tests job
    # (excluding run-all.sh, which ran ngit-helper suites rather than
    # e2e evidence), so a native green covers the same stubs.
    # Fail-fast like the act step (bash -e): the first red suite
    # stops the job and its tail names the failure.
    text = ''
      set -euo pipefail
      say() { printf '===== %s =====\n' "$1"; }
      say "e2e harness stubs (native)"
      bash tests/harness/test-step-grammar.sh
      bash tests/harness/test-baked-set.sh
      bash tests/harness/test-shape-manifest.sh
      bash tests/harness/test-shape-manifest-pipeline.sh
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
    # cargo-audit shells out to the git CLI to clone the advisory DB;
    # coreutils/gnugrep cover what the subcommands shell out to.
    runtimeInputs = [ toolchain ] ++ (with pkgs; [
      coreutils
      gnugrep
      cargo-audit
      cargo-deny
      git
    ]);
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

  # Phase 4: release archive and static site. dist.publish reuses the
  # wyrd-dist tarball realised by the leading build step (same store,
  # no rebuild) and uploads it to public Blossom mirrors with an
  # ephemeral identity: integrity is hash-based, so the uploader key
  # carries no trust. Tag runs only. page.check pins marketing claims
  # to docs with bun; page.deploy publishes via wrangler with the
  # Cloudflare secrets (master pushes only: PR jobs cannot request
  # secrets, so there is no PR preview leg).
  dist.publish = pkgs.writeShellApplication {
    name = "dist-publish";
    runtimeInputs = with pkgs; [ nix git nak ];
    text = ''
      set -euo pipefail
      out=$(nix build .#packages.x86_64-linux.wyrd-dist --print-out-paths --no-link)
      test -f "$out"
      sha=$(nix hash file --type sha256 --base16 "$out")
      echo "artifact=$out"
      echo "sha256=$sha"
      printf '%s' "$sha" > "$NGIT_CI_OUTPUT_DIR/sha256.txt"
      printf '%s' "$(basename "$out")" > "$NGIT_CI_OUTPUT_DIR/artifact.txt"
      # Ephemeral upload identity: integrity is hash-based, so the
      # uploader key carries no trust. The human publisher verifies
      # the sha256 below before referencing it in the release.
      eph=$(nak key generate)
      for server in \
        https://blossom.primal.net \
        https://nostr.download \
        https://cdn.hzrd149.com; do
        echo "--- $server"
        nak blossom upload --server "$server" --sec "$eph" "$out"
      done
    '';
  };

  page.check = pkgs.writeShellApplication {
    name = "page-check";
    runtimeInputs = with pkgs; [ bun ];
    text = ''
      set -euo pipefail
      bun run page/check.ts
    '';
  };

  page.deploy = pkgs.writeShellApplication {
    name = "page-deploy";
    runtimeInputs = with pkgs; [ wrangler coreutils ];
    text = ''
      set -euo pipefail
      CLOUDFLARE_ACCOUNT_ID="$(tr -d '\n' < "$NGIT_CI_SECRETS_DIR/CLOUDFLARE_ACCOUNT")"
      CLOUDFLARE_API_TOKEN="$(tr -d '\n' < "$NGIT_CI_SECRETS_DIR/CLOUDFLARE_API_TOKEN")"
      export CLOUDFLARE_ACCOUNT_ID CLOUDFLARE_API_TOKEN
      wrangler pages deploy page/ --project-name=wyrd-enesis-md
    '';
  };
}
