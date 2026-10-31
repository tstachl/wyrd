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

    services.openssh = {
      enable = true;
      settings.PermitRootLogin = "prohibit-password";
    };
    # Root login is key-only; the key itself arrives per boot, never
    # baked: `wyrd-ssh-key.service` below copies the runner-generated
    # pubkey from the state share into place, so image builds carry
    # no credentials and rotation is just a new run.
    users.users.root.openssh.authorizedKeys.keys = [ ];

    # Boot-time ssh key install from the state share, for root and
    # the e2e suite user. Poll-wait instead of ordering on the
    # virtiofs mount unit: robust regardless of mount timing, and
    # the host retries ssh anyway. set -x: when this unit misbehaves
    # the trace in the journal is the first thing read, and there is
    # no ssh to inspect it with (see wyrd-guest-debug below).
    systemd.services.wyrd-ssh-key = {
      wantedBy = [ "multi-user.target" ];
      script = ''
        set -x
        for i in $(seq 1 150); do
          [ -f /mnt/wyrd-state/authorized_keys.pub ] && break
          sleep 0.2
        done
        mkdir -p /root/.ssh /home/e2e/.ssh
        cp /mnt/wyrd-state/authorized_keys.pub /root/.ssh/authorized_keys
        cp /mnt/wyrd-state/authorized_keys.pub /home/e2e/.ssh/authorized_keys
        chmod 600 /root/.ssh/authorized_keys /home/e2e/.ssh/authorized_keys
        chown -R e2e:users /home/e2e/.ssh
      '';
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
    };

    # Guest-side forensics readable without ssh: the drive evidence
    # task provisions over a host-only tap and ssh may never arrive,
    # so a stuck boot would leave no guest-side evidence unless the
    # guest publishes it itself. This oneshot appends a timestamped
    # bundle to the state share on every boot, mirroring the relay
    # journal dump (same ordering on the virtiofs mount, same
    # temp-file rename, same heartbeat timestamp, same bounded
    # execution). Best effort only: every line tolerates failure
    # and the unit never fails the boot — an absent or stale
    # heartbeat is itself the signal that the dump never completed.
    # Scoped to the drive guest: the e2e trio does not need it, and
    # shared-infra changes stay minimal.
    # A settle delay up front: the dump must show steady state, not
    # a mid-poll race with wyrd-ssh-key's own 30s wait.
    systemd.services.wyrd-guest-debug = lib.mkIf (config.networking.hostName == "wyrd-drive") {
      after = [ "mnt-wyrd\\x2dstate.mount" ];
      requires = [ "mnt-wyrd\\x2dstate.mount" ];
      wantedBy = [ "multi-user.target" ];
      script = ''
        ${pkgs.coreutils}/bin/sleep 45
        LOGDIR=/mnt/wyrd-state/run/logs
        ${pkgs.coreutils}/bin/mkdir -p "$LOGDIR" || exit 0
        {
          echo "=== guest debug $(${pkgs.coreutils}/bin/cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown-boot) $(${pkgs.coreutils}/bin/date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown-time)"
          echo "--- units ---"
          ${pkgs.systemd}/bin/systemctl --no-pager --no-legend list-units 'wyrd-*' 'ssh*' 2>&1 || true
          echo "--- wyrd-ssh-key state ---"
          ${pkgs.systemd}/bin/systemctl --no-pager show wyrd-ssh-key -p ActiveState,SubState,Result,ExecMainStatus,NRestarts 2>&1 || true
          echo "--- wyrd-ssh-key journal (this boot) ---"
          ${pkgs.systemd}/bin/journalctl --no-pager -b -u wyrd-ssh-key 2>&1 | ${pkgs.coreutils}/bin/tail -n 50 || true
          echo "--- state share visibility ---"
          ${pkgs.coreutils}/bin/ls -la /mnt/wyrd-state/ 2>&1 || true
          ${pkgs.coreutils}/bin/ls -la /mnt/wyrd-state/authorized_keys.pub 2>&1 || true
          echo "--- share mount options ---"
          ${pkgs.util-linux}/bin/findmnt -n -o TARGET,OPTIONS /mnt/wyrd-state 2>&1 || ${pkgs.coreutils}/bin/cat /proc/self/mountinfo 2>&1 || true
          echo "--- network ---"
          ${pkgs.iproute2}/bin/ip -brief addr show eth0 2>&1 || true
          echo "--- drive units ---"
          ${pkgs.systemd}/bin/systemctl --no-pager --lines=5 status wyrd-vault-demo wyrd-mount-demo 2>&1 || true
        } > "$LOGDIR/.guest-debug.tmp" 2>&1 || true
        ${pkgs.coreutils}/bin/mv "$LOGDIR/.guest-debug.tmp" "$LOGDIR/guest-debug.log" 2>/dev/null || exit 0
        ${pkgs.coreutils}/bin/date -u +%Y-%m-%dT%H:%M:%SZ > "$LOGDIR/guest-debug.ts" 2>/dev/null || true
        exit 0
      '';
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        TimeoutStartSec = "120s";
      };
    };

    microvm = {
      hypervisor = "qemu";
      shares = [{
        proto = "virtiofs";
        tag = "wyrd-state";
        source = config.wyrd.stateDir;
        mountPoint = "/mnt/wyrd-state";
      }];
    };

    system.stateVersion = "26.05";
  };
}
