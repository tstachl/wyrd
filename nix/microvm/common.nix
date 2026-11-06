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
# other over the test bridge (see peer_signal/peer_wait in
# alpha-common.sh; the per-guest HTTP server is the wyrd-rendezvous
# system unit below).
# Shape and addressing live in gate.box, set per role in
# nix/microvm/{peer-o,peer-n,relay}.nix and owned by the gate
# input's nixosModules.guest: one source for the guest network
# config, the image metadata, and the gate.image/v1 manifest, so the
# three can never drift.
#
# DHCP division: the DHCP client in the guest is Gate-owned and the
# bridge DHCP server is Odin host substrate — neither lives here.
# Wyrd sets values only (gate.box.{mac,ip} per role above, plus the
# explicit `dhcp = true` opt-in each role file carries — the gate
# default is also on, but the opt-in documents intent per guest and
# is immune to default flips). The static ip stays the manifest key
# and the dnsmasq reservation key (MAC → ip), so suite addressing
# (.10/.11/.12) is unchanged. There is deliberately no DHCP code in
# this file (no useNetworkd / systemd.network stanza) — the gate
# guest module owns the client, and a second client here would risk
# a dhcpcd-vs-networkd fight. Requires gate rev 77b3bdc or later
# (the `gate.box.dhcp` option); the flake.lock gate input tracks
# gate master.
{ config, pkgs, lib, ... }:
{
  config = {
    # Hostname derives from the role: wyrd-peer-o, wyrd-peer-n,
    # wyrd-relay. The legs key per-guest state off `hostname -s`.
    networking.hostName = "wyrd-${config.gate.box.role}";

    # 8 GiB virtual disk for every role: the exhaustion leg
    # fallocates ~2.3 GiB at runtime on the guest root fs, which the
    # module default does not hold. Zeros compress, so the tarball
    # stays small.
    gate.image.diskSizeMb = 8192;

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
    # start or stop this server. Serves signals only, never the
    # state root: creds and drives stay off the bridge. The port
    # matches the RENDEZVOUS_PORT the entry publishes (18770); the
    # suite addresses signals through peer_signal/peer_wait in
    # alpha-common.sh.
    systemd.services.wyrd-rendezvous = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        Type = "simple";
        User = "e2e";
        # Root-owned creation path: User= applies to ExecStartPre as
        # well, and e2e cannot create under /var/lib — so the
        # directory setup runs privileged (the `+` prefix) while the
        # server itself stays unprivileged.
        ExecStartPre = "+${pkgs.coreutils}/bin/install -d -o e2e -g users -m 0755 /var/lib/wyrd-e2e/signals";
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
      "wyrd-tests/owner-socket.py".source = ../../tests/microvm/owner-socket.py;
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
