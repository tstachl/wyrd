# nix/microvm/common.nix — shared base for every wyrd Incus guest.
# Fully provisioned at build time: the bridge network has no uplink,
# so guests cannot fetch anything after boot (no `nix profile add`
# inside). The wyrd binary rides the closure too (see the inline
# module in flake.nix): the Incus agent carries control and files,
# and the same content-addressed path serves both sides.
#
# There is no ssh and no shared filesystem: the Incus agent (vsock)
# is the only control plane. The suite reaches guests through
# GATE_GUEST_BIN (guest_exec/push/pull); guests rendezvous with each
# other over the test bridge (see peer_serve in alpha-common.sh).
# Shape and addressing live in options.gate.box, set per role in
# nix/microvm/{peer-o,peer-n,relay}.nix: one source for the guest
# network config, the image metadata, and the gate.image/v1 manifest
# (see nix/microvm/image.nix), so the three can never drift.
{ config, pkgs, lib, ... }:
{
  options.gate.box = {
    # Manifest role: names the instance in gate runs and addresses
    # it in the suite (peer-o, peer-n, relay).
    role = lib.mkOption {
      type = lib.types.strMatching "^[a-z0-9][a-z0-9-]{0,31}$";
      description = "Incus image role (gate.image/v1).";
    };
    vcpu = lib.mkOption {
      type = lib.types.ints.between 1 64;
      description = "Guest vcpus, checked against the host ceiling at launch.";
    };
    memMb = lib.mkOption {
      type = lib.types.ints.between 512 131072;
      description = "Guest memory in MiB, checked against the host ceiling at launch.";
    };
    mac = lib.mkOption {
      type = lib.types.strMatching "^([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$";
      description = "Guest NIC hardware address on the managed bridge.";
    };
    # Bare dotted quad; the /24 suffix is appended where the wire
    # format needs it (network prefix length, manifest ip). The
    # bridge prefix itself is host policy (create-once), and the
    # provisioner refuses a manifest outside it.
    ip = lib.mkOption {
      type = lib.types.strMatching "^([0-9]{1,3}\\.){3}[0-9]{1,3}$";
      description = "Guest static IPv4 address (bare, /24 assumed).";
    };
  };

  config = {
    # Hostname derives from the role: wyrd-peer-o, wyrd-peer-n,
    # wyrd-relay. The legs key per-guest state off `hostname -s`.
    networking.hostName = "wyrd-${config.gate.box.role}";
    # Deterministic interface names: with one NIC per guest this is
    # always eth0, so the suite throttles and addresses one name.
    boot.kernelParams = [ "net.ifnames=0" ];
    # Static address on the managed bridge. DHCP (from the bridge's
    # dnsmasq) may add a second address; the static one below is what
    # the suite dials and what the manifest declares.
    networking.interfaces.eth0.ipv4.addresses =
      [{ address = config.gate.box.ip; prefixLength = 24; }];

    # The test bridge is isolated (no uplink, host-only): filtering
    # there buys nothing and breaks the relay port plus dynamic iroh
    # UDP between peers. The VM boundary is the isolation.
    networking.firewall.trustedInterfaces = [ "eth0" ];

    # The spike proved /dev/fuse is rw under the default microvm
    # kernel; no extra kernel modules needed. The suite fails loudly
    # at first mount if that ever regresses.
    # loop: the exhaustion leg loop-mounts a fixed-size ext4 image
    # in-guest so the fills can never reach the host disk.
    boot.kernelModules = [ "loop" ];
    environment.systemPackages = with pkgs; [
      python3Minimal
      jq
      iproute2
      fuse3
      sudo
      procps
      e2fsprogs
      util-linux
      # `wyrd update` unpacks release archives by shelling out to
      # system tar: the post-upgrade convergence leg needs it in-guest.
      gnutar
      # The upgrade archives are .tar.gz: unpacking shells out to
      # system gzip alongside tar.
      gzip
    ];
    security.sudo.enable = true;
    # The suite runs as e2e, mirroring the Lima guest's unprivileged
    # user: the credential-hardening negatives (wrong owner,
    # group-readable) only mean anything as non-root. The uid is
    # pinned, not auto-allocated, to keep file ownership stable
    # across image builds; nothing crosses the host boundary as
    # files anymore (agent transport), so no host uid must match.
    # NixOS forbids sub-1000 uids for normal users, so e2e is a
    # system user with the interactive bits set explicitly (home,
    # shell, wheel for the suite's passwordless sudo). Passwordless
    # sudo covers the suite's `sudo tc` / `sudo chown` calls plus
    # the exhaustion leg's `sudo mkfs.ext4` and `sudo mount -o loop`.
    users.users.e2e = {
      isSystemUser = true;
      uid = 981;
      group = "users";
      home = "/home/e2e";
      createHome = true;
      shell = pkgs.bashInteractive;
      extraGroups = [ "wheel" ];
    };
    # /dev/fuse is 0666 via systemd's default udev rules, and the
    # setuid fusermount wrapper below lets e2e mount.
    security.sudo.extraRules = [{
      users = [ "e2e" ];
      commands = [{ command = "ALL"; options = [ "NOPASSWD" ]; }];
    }];
    programs.fuse.userAllowOther = true;

    # Run-scoped cross-guest rendezvous: every guest serves its
    # signals dir over HTTP from boot (stdlib only), so a signal a
    # leg writes stays fetchable after that leg exits — legs never
    # start or stop this server, and the entry never touches it.
    # Serves signals only, never the state root: creds and drives
    # stay off the bridge. The port matches the RENDEZVOUS_PORT the
    # entry publishes (18770); the suite addresses signals through
    # peer_signal/peer_wait in alpha-common.sh.
    systemd.services.wyrd-rendezvous = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        Type = "simple";
        User = "e2e";
        ExecStartPre = "${pkgs.coreutils}/bin/mkdir -p /var/lib/wyrd-e2e/signals";
        ExecStart = "${pkgs.python3Minimal}/bin/python3 -m http.server 18770 --directory /var/lib/wyrd-e2e/signals";
        Restart = "always";
        RestartSec = "5s";
      };
    };

    # The suite rides the image, not a share: guests have no uplink,
    # so nothing can be fetched after boot. Paths are relative to
    # this file (nix/microvm/) into the repo's tests/.
    environment.etc = {
      "wyrd-tests/alpha-common.sh".source = ../../tests/lima/alpha-common.sh;
      "wyrd-tests/alpha-lima.sh".source = ../../tests/lima/alpha-lima.sh;
      "wyrd-tests/alpha-microvm-legs.sh".source = ../../tests/microvm/alpha-microvm-legs.sh;
      "wyrd-tests/alpha-lima-matrix.py".source = ../../tests/lima/alpha-lima-matrix.py;
      # Sourced at runtime by alpha-lima.sh (step selection); without it
      # every microVM phase 1 dies on a missing file before the first
      # check. Keep this set in sync with what the guest-run scripts
      # source.
      "wyrd-tests/step-grammar.sh".source = ../../tests/harness/step-grammar.sh;
      # Wrapper-only entry enforcement: sourced by every suite script
      # above before anything else. Same sync rule.
      "wyrd-tests/e2e-guard.sh".source = ../../tests/harness/e2e-guard.sh;
    };

    system.stateVersion = "26.05";
  };
}
