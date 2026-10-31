# nix/microvm/image.nix — Incus image assembly for one e2e role.
#
# Produces system.build.wyrdImage: an output directory holding
# exactly manifest.json (the gate.image/v1 contract, the single
# declaration of the image: role, shape, mac, ip) and image.tar.gz
# (the unified Incus tarball the backend imports: metadata.yaml +
# rootfs.img). One build gives image + shape, so the manifest can
# never drift from the image it describes: both derive from
# config.gate.box (see common.nix), and the derivation re-validates
# the manifest fail-closed at build time, mirroring the host's
# prov_load_manifest (kind, key set, types, bounds).
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
in
{
  config = {
    system.build.wyrdQemuImage = wyrdQemuImage;
    system.build.wyrdImage =
      pkgs.runCommand "wyrd-image-${box.role}"
        {
          nativeBuildInputs = [ pkgs.jq pkgs.gnutar pkgs.gzip ];
          inherit manifestFile metadataFile wyrdQemuImage;
          # Bake the expectations in, not just the values: the
          # derivation refuses instead of shipping a manifest the
          # host would refuse at launch.
          passAsFile = [ ];
        }
        ''
          mkdir -p "$out/stage"
          cp "$manifestFile" "$out/manifest.json"
          cp "$metadataFile" "$out/stage/metadata.yaml"
          cp "${wyrdQemuImage}/nixos.qcow2" "$out/stage/rootfs.img"
          # Build-time contract check, mirroring the host provisioner
          # (wrapper/provision-host.sh prov_load_manifest) and the
          # published schema (wrapper/image-manifest.schema.json):
          # kind exact, key set exact (order-proof set difference
          # plus arity, like the host), typed bounds. The bridge
          # prefix itself is host policy and is checked at launch,
          # not here.
          jq -e '.kind == "gate.image/v1"' "$out/manifest.json" > /dev/null
          jq -e '(([keys_unsorted[]] - ["kind","role","vcpu","memMb","mac","ip","shares"] | length) == 0) and ((keys | length) == 7)' "$out/manifest.json" > /dev/null
          jq -e '(.vcpu | type) == "number" and (.vcpu | floor) == .vcpu and .vcpu >= 1 and .vcpu <= 64' "$out/manifest.json" > /dev/null
          jq -e '(.memMb | type) == "number" and (.memMb | floor) == .memMb and .memMb >= 512 and .memMb <= 131072' "$out/manifest.json" > /dev/null
          jq -e '(.role | type) == "string" and (.role | test("^[a-z0-9][a-z0-9-]{0,31}$"))' "$out/manifest.json" > /dev/null
          jq -e '(.mac | type) == "string" and (.mac | test("^([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$"))' "$out/manifest.json" > /dev/null
          jq -e '(.ip | type) == "string" and (.ip | test("^([0-9]{1,3}\\.){3}[0-9]{1,3}/24$"))' "$out/manifest.json" > /dev/null
          jq -e '(.shares | type) == "array" and (.shares | length) == 0' "$out/manifest.json" > /dev/null
          # Reproducible tarball: sorted entries, fixed mtime, no
          # gzip filename/timestamp, so equal inputs publish identical
          # bytes (the host imports under a content-hash alias).
          ${pkgs.gnutar}/bin/tar --sort=name --mtime=@1 \
            -I '${pkgs.gzip}/bin/gzip -n' -cf "$out/image.tar.gz" \
            -C "$out/stage" metadata.yaml rootfs.img
          rm -rf "$out/stage"
          [ "$(ls -A "$out" | sort | tr '\n' ' ')" = "image.tar.gz manifest.json " ]
        '';
  };
}
