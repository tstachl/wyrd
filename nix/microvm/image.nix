# nix/microvm/image.nix — Incus image assembly for one e2e role.
#
# Produces system.build.wyrdImage: an output directory holding
# exactly manifest.json (the gate.image/v1 contract, the single
# declaration of the image: role, shape, mac, ip) and image.tar.gz
# (the unified Incus tarball the backend imports: metadata.yaml +
# rootfs.img). One build gives image + shape, so the manifest can
# never drift from the image it describes: both derive from
# config.gate.box (see common.nix), and the derivation validates the
# manifest fail-closed at build time against the vendored gate
# schema (nix/microvm/image-manifest.schema.json), so the bounds the
# build enforces are the schema's, not a hand copy.
#
# The vendored schema file is a verbatim copy of the gate repo's
# wrapper/image-manifest.schema.json (gate rev 53b1b7b); refresh it
# when the gate contract moves. A manifest that violates the
# vendored schema fails the build here; a gate-side schema change
# the vendored copy does not have is invisible to the build and is
# caught by the host at launch instead (fail closed there, never
# false green).
#
# The qcow2 comes from the same make-disk-image the upstream
# incus-virtual-machine module uses, with an explicit virtual size:
# the exhaustion leg fallocates ~2.3 GiB at runtime on the guest
# root fs, which "auto" (closure + 512M) cannot hold. 8 GiB virtual
# costs ~nothing in the tarball (sparse zeros compress away).
{ config, pkgs, lib, ... }:
let
  box = config.gate.box;

  manifest = {
    kind = "gate.image/v1";
    role = box.role;
    vcpu = box.vcpu;
    memMb = box.memMb;
    mac = box.mac;
    ip = "${box.ip}/24";
    shares = [ ];
  };
  manifestFile = pkgs.writeText "manifest.json" (builtins.toJSON manifest);

  metadataFile = pkgs.writeText "metadata.yaml" ''
    architecture: ${pkgs.stdenv.hostPlatform.qemuArch}
    creation_date: 1
    properties:
      os: NixOS
      release: "26.05"
      description: "wyrd e2e ${box.role}"
  '';

  # Same disk layout the upstream incus module builds, sized for the
  # suite (see above). Not system.build.qemuImage: that stays at
  # "auto" for whatever else references it, and nothing else does.
  wyrdQemuImage = import "${pkgs.path}/nixos/lib/make-disk-image.nix" {
    inherit pkgs lib config;
    diskSize = 8192;
    partitionTableType = "efi";
    format = "qcow2-compressed";
    copyChannel = config.system.installer.channel.enable;
  };

  # Schema-driven manifest validation (stdlib only): every bound the
  # build enforces is read from the vendored schema file by
  # nix/microvm/validate-manifest.py — kind const, required key set,
  # per-field types/patterns/minima/maxima. A gate-side schema change
  # the vendored copy does not have fails the build here.
  validateManifest = ./validate-manifest.py;
in
{
  config = {
    system.build.wyrdQemuImage = wyrdQemuImage;
    system.build.wyrdImage =
      pkgs.runCommand "wyrd-image-${box.role}"
        {
          nativeBuildInputs = [ pkgs.python3Minimal pkgs.gnutar pkgs.gzip ];
          inherit manifestFile metadataFile wyrdQemuImage validateManifest;
          schemaFile = ./image-manifest.schema.json;
        }
        ''
          mkdir -p "$out/stage"
          cp "$manifestFile" "$out/manifest.json"
          cp "$metadataFile" "$out/stage/metadata.yaml"
          cp "${wyrdQemuImage}/nixos.qcow2" "$out/stage/rootfs.img"
          python3 "$validateManifest" "$out/manifest.json" "$schemaFile"
          # Reproducible tarball: sorted entries, fixed mtime and
          # ownership, no gzip filename/timestamp, so equal inputs
          # publish identical bytes (the host imports under a
          # content-hash alias).
          ${pkgs.gnutar}/bin/tar --sort=name --mtime=@1 --owner=0 --group=0 --numeric-owner \
            -I '${pkgs.gzip}/bin/gzip -n' -cf "$out/image.tar.gz" \
            -C "$out/stage" metadata.yaml rootfs.img
          rm -rf "$out/stage"
          [ "$(ls -A "$out" | sort | tr '\n' ' ')" = "image.tar.gz manifest.json " ]
        '';
  };
}
