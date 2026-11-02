{
  description = "Wyrd: decentralized, append-only, content-addressed drive system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Subject-tree input for the shape-pinned E2E gate (step 1 seam).
    # Default: a committed empty placeholder, which the call site maps
    # to null (this flake's own build — today's behavior, byte for
    # byte). The host supplies a real subject with
    # `--override-input wyrdSrc <tree>`: the override is resolved
    # before pure evaluation begins, so everything downstream stays
    # pure, restricted, and IFD-free. A subject.nix-style scratch file
    # cannot serve here: flake evaluation never sees untracked files,
    # and absolute path literals (even /nix/store) are forbidden in
    # pure mode — the input system is the only sanctioned channel for
    # external trees.
    wyrdSrc.url = "path:./nix/microvm/empty-src";
    wyrdSrc.flake = false;
    # Hardened e2e gate: two peers plus a relay as Incus VM images
    # (see nix/microvm/). x86_64-linux only: KVM hosts are x86_64,
    # and image assembly (disk images) builds on Linux. The subject
    # wyrd package rides the image closure; the manifest next to the
    # tarball declares the image's shape. The image contract
    # (gate.box options, manifest generator, assembly) is owned by
    # the gate input's nixosModules.guest; this repo declares values
    # and guest config only.
    gate.url = "git+https://grasp.t5.st/npub1k0y4eceal2zryes3azm6nsgt0r0jsa2v8zcsdf9uqxttn0jlfe9q04c9h8/gate.git?ref=refs/heads/master";
    gate.inputs.nixpkgs.follows = "nixpkgs";
    # Service-module framework for the declarative drive module
    # (34): pinned to the 26.05 release to match nixpkgs, same as
    # every other input follows the same channel.
    darwin.url = "github:lnl7/nix-darwin/nix-darwin-26.05";
    darwin.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, crane, rust-overlay, darwin, wyrdSrc, gate, ... }:
    let
      # Product platforms. x86_64-darwin is deliberately absent even though
      # rust-toolchain.toml lists the target: no builder covers it, so it
      # ships nothing until one does.
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      # ngit release platform tags, which order OS before architecture.
      ngitPlatforms = {
        aarch64-darwin = "macos-aarch64";
        x86_64-darwin = "macos-x86_64";
        aarch64-linux = "linux-aarch64";
        x86_64-linux = "linux-x86_64";
      };
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      pkgsFor = system:
        import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
      wyrdFor = system: src:
        { pname ? "wyrd"
        , mainProgram ? "wyrd"
        , cargoExtraArgs ? "-p wyrd-cli"
        , description ? "Decentralized, append-only, content-addressed drive system (alpha)"
        }:
        let
          pkgs = pkgsFor system;
          inherit (pkgs) lib stdenv;
          # The exact channel pinned in ./rust-toolchain.toml, so the flake
          # build compiles with the same rustc as the devenv shell.
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
          # The version travels with the source tree, not the flake: a
          # host-supplied wyrdSrc names its own version the same way.
          # Tolerant read (N1): workspace roots carry
          # [workspace.package], but a usable subject only promises a
          # version somewhere standard — fall back to [package] before
          # failing loudly at this layer (not deep inside crane).
          manifest = builtins.fromTOML (builtins.readFile (src + "/Cargo.toml"));
          version = manifest.workspace.package.version or manifest.package.version or (throw "wyrdSrc Cargo.toml has neither [workspace.package] nor [package] version: ${toString src}");
          # One binary per package: the user-facing CLI and the
          # developer-run desktop host (06, headless owner process for
          # the Lima order-evidence leg). Library crates ride along as
          # dependencies; nothing else installs a binary.
          binFor = pname: mainProgram: cargoExtraArgs: description:
            craneLib.buildPackage {
              inherit pname version cargoExtraArgs;
              # The workspace exactly as Cargo sees it, from the committed
              # Cargo.lock; target/ and VCS state never enter the build.
              src = craneLib.cleanCargoSource src;
              strictDeps = true;
              # The test suite (nextest, doctests) runs in rust-ci with the full
              # dev environment; the distribution build only proves the binary
              # compiles and links, including inside the Linux sandbox.
              doCheck = false;
              nativeBuildInputs = [ pkgs.pkg-config ];
              # System libraries probed by build scripts (fuser's pkg-config
              # probe) belong in buildInputs, not nativeBuildInputs: under
              # strictDeps the pkg-config role hook only exposes host-offset
              # deps on the unsuffixed PKG_CONFIG_PATH the probe reads.
              buildInputs = lib.optionals stdenv.isDarwin [
                # Build-time headers/stubs for fuser's libfuse probes
                # (fuse.pc from nixpkgs, fuse3.pc from our own stubs:
                # nix/macfuse3-stubs.nix, same split as devenv.nix).
                # Mounting at runtime still needs system macFUSE, which nix
                # cannot provide.
                pkgs.macfuse-stubs
                (pkgs.callPackage ./nix/macfuse3-stubs.nix { })
              ] ++ lib.optionals stdenv.isLinux [
                # libfuse2 headers for fuser's link probe.
                pkgs.fuse
              ];
              meta = with lib; {
                inherit description;
                homepage = "https://gitworkshop.dev";
                license = licenses.mit;
                platforms = systems;
                inherit mainProgram;
              };
            };
          # One binary per package: the CLI today, the developer-run host
          # beside it. Callers pass a set; omitted fields default to the
          # user-facing CLI build.
        in
        binFor pname mainProgram cargoExtraArgs description;
      # Deterministic release archive for one system: the tarball named in
      # .ngit/release.yaml for that system's platform tag. The release
      # procedure builds one per platform and publishes the set, so a main
      # release always covers every application platform.
      distFor = system:
        let
          pkgs = pkgsFor system;
          platform = ngitPlatforms.${system};
          dirname = "wyrd-${version}-${platform}";
        in
        # Explicit store paths, not PATH: sandbox tar/gzip vary by platform
          # (BSD tar on darwin lacks --sort-name), and the archive format
          # must not.
        pkgs.runCommand "${dirname}.tar.gz" { }
          ''
            mkdir -p staging/${dirname}/bin
            cp ${self.packages.${system}.wyrd}/bin/wyrd staging/${dirname}/bin/
            # Reproducible archive: sorted entries, fixed mtime, zero
            # owner/group, and no gzip filename/timestamp, so every builder
            # publishes identical bytes. Re-verify with
            # `nix build .#wyrd-dist --rebuild` after touching this.
            ${pkgs.gnutar}/bin/tar --sort=name --mtime=@1 --owner=0 --group=0 \
              --numeric-owner -I '${pkgs.gzip}/bin/gzip -n' -cf $out \
              -C staging ${dirname}
          '';
    in
    {
      packages = forAllSystems (system: {
        wyrd = wyrdFor system ./. { };
        # Developer-run desktop host (06): the headless owner process
        # for the Lima order-evidence leg. Never distributed (07 owns
        # the shipped artifact); the flake contract pins only that
        # `-p wyrd-cli` stays the distribution build.
        wyrd-app = wyrdFor system ./. {
          pname = "wyrd-app";
          mainProgram = "wyrd-app";
          cargoExtraArgs = "-p wyrd-app";
          description = "Desktop host owner process (developer-run, headless)";
        };
        wyrd-dist = distFor system;
        # Pinned relay-query tool for the gate opacity probe
        # (gate-phase-03): reproducibility is the reason — the suite
        # runs the wyrd image's own nak beside WYRD_BIN instead of
        # floating nixpkgs#nak.
        nak = (pkgsFor system).nak;
        # Supervisor wrapper the drive-service module inlines
        # (nix/drive/{darwin,nixos}.nix `readFile` this same script):
        # exposed so system-level evidence can build the exact
        # artifact the module names — evaluation alone leaves an
        # unrealized store path that dies silently at exec.
        # Same builder, same source, same nixpkgs: the leg asserts
        # the output path equals the module's argv[0] rather than
        # assuming it.
        wyrd-supervise = (pkgsFor system).writers.writePython3Bin "wyrd-supervise" { } (
          builtins.readFile ./nix/drive/supervise.py
        );
        default = self.packages.${system}.wyrd;
      });
      apps = forAllSystems (system: {
        wyrd = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${system}.wyrd;
          meta = {
            description = "Run the wyrd drive daemon (init, mount)";
          };
        };
        default = self.apps.${system}.wyrd;
      });
      # The package build plus the release archive, per system, plus
      # the rust gates the native CI builds (compile-once shape: deps
      # once per lockfile, each gate only workspace crates).
      # `nix flake check` builds the current system's entries; CI covers
      # linux, the maintainer's machine darwin.
      # On x86_64-linux the Incus images join the gate as
      # evaluation only (CI runs `nix flake check --no-build`): they
      # prove the guest configs evaluate (gate.box shape, image
      # assembly) without assembling disk images on every PR. The
      # images build when a gate run needs them.
      # Suite *execution* needs KVM and runs on the maintainer's Linux
      # host via the gate task (tests/gate/gate-entry.sh as the build user
      # under host-owned provisioning), never in CI.
      checks = forAllSystems (system: {
        wyrd = self.packages.${system}.wyrd;
        wyrd-dist = self.packages.${system}.wyrd-dist;
      } // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") ({
        wyrd-image-peer-o = self.images.peer-o;
        wyrd-image-peer-n = self.images.peer-n;
        wyrd-image-relay = self.images.relay;
        # The opacity probe's relay-query tool rides the images, but
        # attest the package attribute the images resolve it from,
        # on the platform that runs it.
        wyrd-image-nak = self.packages.${system}.nak;
        # Rust gate derivations for the native CI (compile-once
        # shape): rust-deps compiles third-party and
        # dev-dependencies once per Cargo.lock; each gate compiles
        # only workspace crates against those artifacts. Flat rust-*
        # names: `nix flake check` requires every checks entry to be a
        # derivation. The coordinator realises these for rust.yaml's
        # build jobs.
      } // import ./.ngit/nix/builds {
        pkgs = pkgsFor "x86_64-linux";
        inherit crane;
        src = ./.;
      }));
      # Declarative drive services (34): one module, two modes, one
      # declaration per drive. The same file serves NixOS and
      # nix-darwin, selected on `stdenv` inside.
      nixosModules.wyrd-drive = import ./nix/drive/nixos.nix;
      darwinModules.wyrd-drive = import ./nix/drive/darwin.nix;
      nixosModules.default = self.nixosModules.wyrd-drive;
      darwinModules.default = self.darwinModules.wyrd-drive;
      # Module evaluation fixtures for the shell gate
      # (tests/harness/test-wyrd-module.sh): minimal machines declaring one
      # vault and one mount each, evaluated but never built. The
      # NixOS fixture joins nixosConfigurations below; the Darwin
      # fixture is the top-level darwinConfigurations.
      darwinConfigurations.wyrd-drive-test = darwin.lib.darwinSystem {
        system = "aarch64-darwin";
        modules = [
          self.darwinModules.wyrd-drive
          (
            { pkgs, ... }:
            {
              system.stateVersion = 6;
              services.wyrd.drives = {
                vault-demo = {
                  enable = true;
                  driveDir = "/var/lib/wyrd/demo";
                  mode = "vault";
                  identityFile = "/etc/wyrd/identity";
                  passphraseFile = "/etc/wyrd/passphrase";
                  relays = [ "wss://relay.example" ];
                  package = self.packages.aarch64-darwin.wyrd;
                };
                mount-demo = {
                  enable = true;
                  driveDir = "/var/lib/wyrd/laptop";
                  mode = "mount";
                  mountpoint = "/Volumes/Wyrd";
                  identityFile = "/etc/wyrd/identity";
                  package = self.packages.aarch64-darwin.wyrd;
                };
              };
            }
          )
        ];
      };
      # Hardened e2e images: two peers plus a relay, each an Incus VM
      # image definition (see nix/microvm/). x86_64-linux only: KVM
      # hosts are x86_64. Each guest carries a wyrd package as its
      # subject: by default this flake's build, optionally a
      # host-supplied source tree via --override-input wyrdSrc. The
      # subject never influences evaluation of the harness itself.
      nixosConfigurations =
        let
          # The wyrdSrc input is a committed placeholder FILE by
          # default (./nix/microvm/empty-src — a file, not a
          # directory, so the cargo source filter drops it; it
          # contributed no bytes at introduction, and later lockfile
          # changes move the package independently of the seam). A placeholder file
          # maps to null: this flake's own build. Anything else must
          # be a host-supplied subject tree carrying Cargo.toml at
          # root (wyrdFor reads its version); anything unusable fails
          # loudly here, never silently. Only the placeholder's exact
          # bytes map to null, and those bytes cannot build wyrd — so
          # the fallback can never mask a real subject. (Existence
          # probes only: readFile runs solely on paths already proven
          # to be files, so no IO error can escape — every bad input
          # lands on the throw below with a readable message.)
          wyrdSrcFor =
            let
              hasCargo = builtins.pathExists (wyrdSrc + "/Cargo.toml");
              isDir = builtins.pathExists (wyrdSrc + "/.");
              isPlaceholder =
                !isDir && builtins.readFile wyrdSrc == "empty-subject-placeholder\n";
            in
            if hasCargo
            then wyrdSrc
            else if isPlaceholder
            then null
            else throw "wyrdSrc subject unreadable: expected a source tree with Cargo.toml at root, or the default placeholder; got ${toString wyrdSrc}";
          # imageGuestFor takes wyrdSrc with no default (N4): an
          # omitted argument must never silently mean the repo's own
          # build. Every call site passes explicitly.
          # extraModules extends the guest composition for role
          # guests that carry more than the suite (the drive-service
          # evidence guest adds the wyrd-drive module and its
          # declarations); the e2e trio passes none.
          imageGuestFor = { role, wyrdSrc, extraModules ? [ ] }:
            let
              wyrdPkg =
                if wyrdSrc == null
                then self.packages.x86_64-linux.wyrd
                else wyrdFor "x86_64-linux" wyrdSrc { };
            in
            nixpkgs.lib.nixosSystem {
              system = "x86_64-linux";
              modules = [
                "${nixpkgs}/nixos/modules/virtualisation/incus-virtual-machine.nix"
                gate.nixosModules.guest
                ./nix/microvm/common.nix
                ./nix/microvm/${role}.nix
                # The suite executes WYRD_BIN in-guest over the agent,
                # and the opacity probe runs nak in-guest too — so the
                # guests carry both packages. Stable paths
                # (/run/current-system/sw/bin/...) serve both sides,
                # no handoff rewriting needed.
                ({ pkgs, ... }: {
                  environment.systemPackages =
                    [ wyrdPkg pkgs.nak ];
                })
              ] ++ extraModules;
            };
          imageGuest = role: imageGuestFor { inherit role; wyrdSrc = wyrdSrcFor; };
        in
        {
          wyrd-peer-o = imageGuest "peer-o";
          wyrd-peer-n = imageGuest "peer-n";
          wyrd-relay = imageGuest "relay";
          # Module evaluation fixture for the shell gate
          # (tests/harness/test-wyrd-module.sh): one vault and one mount,
          # evaluated but never built.
          wyrd-drive-test = nixpkgs.lib.nixosSystem {
            system = "x86_64-linux";
            modules = [
              self.nixosModules.wyrd-drive
              (
                { pkgs, ... }:
                {
                  nixpkgs.hostPlatform = "x86_64-linux";
                  system.stateVersion = "26.05";
                  # Minimal bootable-system scaffolding: the fixture is
                  # evaluated but never built or booted (CI runs
                  # check with --no-build). `nix flake check`
                  # evaluates each configuration's toplevel, whose
                  # assertions require these two options.
                  fileSystems."/" = {
                    device = "/dev/disk/by-label/nixos";
                    fsType = "ext4";
                  };
                  boot.loader.grub.devices = [ "nodev" ];
                  services.wyrd.drives = {
                    vault-demo = {
                      enable = true;
                      driveDir = "/var/lib/wyrd/demo";
                      mode = "vault";
                      identityFile = "/etc/wyrd/identity";
                      passphraseFile = "/etc/wyrd/passphrase";
                      relays = [ "wss://relay.example" ];
                      logFile = "/var/log/wyrd-demo.log";
                      package = self.packages.x86_64-linux.wyrd;
                    };
                    mount-demo = {
                      enable = true;
                      driveDir = "/var/lib/wyrd/laptop";
                      mode = "mount";
                      mountpoint = "/mnt/wyrd";
                      identityFile = "/etc/wyrd/identity";
                      passphraseFile = "/etc/wyrd/passphrase";
                      package = self.packages.x86_64-linux.wyrd;
                    };
                  };
                }
              )
            ];
          };
          # Bootable drive-service evidence guest for the system-level
          # execution issue: the same vault+mount declaration as
          # wyrd-drive-test, composed onto the Incus image guest path
          # so the gate task can provision and boot it like the e2e
          # trio. Evaluated in CI like the fixture; booted only by
          # the drive-service task on the KVM host, never in CI.
          wyrd-drive-vm = imageGuestFor {
            role = "drive";
            wyrdSrc = wyrdSrcFor;
            extraModules = [
              self.nixosModules.wyrd-drive
              (
                { ... }:
                {
                  system.stateVersion = "26.05";
                  services.wyrd.drives = {
                    vault-demo = {
                      enable = true;
                      driveDir = "/var/lib/wyrd/demo";
                      mode = "vault";
                      identityFile = "/etc/wyrd/identity";
                      passphraseFile = "/etc/wyrd/passphrase";
                      relays = [ "wss://relay.example" ];
                      logFile = "/var/log/wyrd-demo.log";
                      package = self.packages.x86_64-linux.wyrd;
                    };
                    mount-demo = {
                      enable = true;
                      driveDir = "/var/lib/wyrd/laptop";
                      mode = "mount";
                      mountpoint = "/mnt/wyrd";
                      identityFile = "/etc/wyrd/identity";
                      passphraseFile = "/etc/wyrd/passphrase";
                      package = self.packages.x86_64-linux.wyrd;
                    };
                  };
                }
              )
            ];
          };
        };
      # Bootable Incus images for the gate task, one output per role.
      # Each output dir holds exactly manifest.json (gate.image/v1)
      # and image.tar.gz (the unified tarball the host imports), both
      # assembled by the gate-owned guest module from the same
      # gate.box options (see nix/microvm/).
      # x86_64-linux only: KVM hosts are x86_64, and disk assembly
      # builds on Linux (evaluation works anywhere).
      images = {
        peer-o = self.nixosConfigurations.wyrd-peer-o.config.system.build.gateImage;
        peer-n = self.nixosConfigurations.wyrd-peer-n.config.system.build.gateImage;
        relay = self.nixosConfigurations.wyrd-relay.config.system.build.gateImage;
        drive = self.nixosConfigurations.wyrd-drive-vm.config.system.build.gateImage;
      };
      # Native CI commands (ngit-ci Nix path): one group per workflow
      # file under .ngit/nix/workflows, exposed per system. Only
      # x86_64-linux until another provider exists; the coordinator
      # resolves `run` references against these (see the workflow docs
      # in the ngit-ci repo). Act workflows are untouched.
      ngit-ci.commands.x86_64-linux = import ./.ngit/nix/workflows {
        pkgs = pkgsFor "x86_64-linux";
        inherit crane;
        src = ./.;
      };
    };
}
