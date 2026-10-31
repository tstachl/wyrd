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
# The schema file is a verbatim copy of the gate repo's
# wrapper/image-manifest.schema.json (gate rev 53b1b7b); refresh it
# when the gate contract moves. Staleness degrades to
# build-pass/launch-refuse, never false green: the host provisioner
# re-validates every manifest at launch.
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
  # build enforces is read from nix/microvm/image-manifest.schema.json
  # — kind const, required key set with no additional properties,
  # per-field types, patterns, minima/maxima. A schema change the
  # vendored copy does not have fails here, not at launch.
  validateManifest = pkgs.writeText "validate-manifest.py" ''
    import json, re, sys
    manifest = json.load(open(sys.argv[1]))
    schema = json.load(open(sys.argv[2]))
    props = schema["properties"]
    assert manifest.get("kind") == props["kind"]["const"], \
        "kind is not %r" % props["kind"]["const"]
    assert set(manifest) == set(schema["required"]), \
        "keys are not exactly %s" % sorted(schema["required"])
    for key in schema["required"]:
        spec, val = props[key], manifest[key]
        t = spec.get("type")
        if t == "string":
            assert isinstance(val, str), "%s is not a string" % key
        elif t == "integer":
            assert isinstance(val, int) and not isinstance(val, bool), \
                "%s is not an integer" % key
        elif t == "array":
            assert isinstance(val, list), "%s is not an array" % key
        else:
            raise AssertionError("unsupported schema type for %s: %s" % (key, t))
        if "pattern" in spec:
            assert re.search(spec["pattern"], val), \
                "%s does not match %s" % (key, spec["pattern"])
        if "minimum" in spec:
            assert val >= spec["minimum"], "%s below minimum" % key
        if "maximum" in spec:
            assert val <= spec["maximum"], "%s above maximum" % key
        if "maxItems" in spec:
            assert len(val) <= spec["maxItems"], "%s has too many items" % key
    print("manifest matches %s" % schema["$id"])
  '';
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
