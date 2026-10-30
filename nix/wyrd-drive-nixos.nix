# NixOS half of the declarative Wyrd drive services: systemd units
# plus the switch-time conflict probe. Imports the shared option
# surface; the darwin half lives beside this file. Secrets are
# paths, never store strings (asserted in the options file).
{ config, lib, pkgs, ... }:

let
  enabled = lib.filterAttrs (_: drive: drive.enable) config.services.wyrd.drives;

  supervise = pkgs.writers.writePython3Bin "wyrd-supervise" { } (
    builtins.readFile ./wyrd-supervise.py
  );
  probe = pkgs.writers.writePython3Bin "wyrd-drive-probe" { } (
    builtins.readFile ./wyrd-drive-probe.py
  );

  posturePath = drive:
    "${toString drive.driveDir}/${if drive.mode == "vault" then "vault.posture" else "mount.posture"}";

  wyrdArgs = drive:
    [ "${drive.package}/bin/wyrd" drive.mode (toString drive.driveDir) ]
    ++ lib.optionals (drive.mode == "mount") [ (toString drive.mountpoint) ]
    ++ [ "--identity-file" (toString drive.identityFile) ]
    ++ lib.optionals (drive.passphraseFile != null) [
      "--passphrase-file"
      (toString drive.passphraseFile)
    ]
    ++ lib.concatMap (relay: [ "--relay" relay ]) drive.relays
    ++ lib.optionals (drive.mode == "vault" && drive.logFile != null) [
      "--log-file"
      (toString drive.logFile)
    ];

  # The wrapper's exit statuses are the supervisor contract (OD-34-C):
  # 78 is terminal (never restarted), everything else restarts
  # rate-limited. systemd restarts non-zero but not clean exits, so
  # the restart-worthy status is 1.
  wrapperArgv = drive:
    [ "${supervise}/bin/wyrd-supervise" "--restart-status=1" "--posture-path=${posturePath drive}" "--" ]
    ++ wyrdArgs drive;
in
{
  imports = [ ./wyrd-drive-options.nix ];

  systemd.services = lib.mapAttrs'
    (
      name: drive:
        lib.nameValuePair "wyrd-${name}" {
          description = "Wyrd drive ${name} (${drive.mode})";
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          wantedBy = [ "multi-user.target" ];
          startLimitIntervalSec = 300;
          startLimitBurst = 3;
          serviceConfig = {
            Type = "simple";
            ExecStart = lib.concatStringsSep " " (wrapperArgv drive);
            # Recover from unexpected termination, never from a
            # configuration error: 78 is the wrapper's terminal status,
            # excluded here; everything else restarts rate-limited by the
            # start limit above.
            Restart = "on-failure";
            RestartPreventExitStatus = [ 78 ];
            RestartSec = 5;
            NoNewPrivileges = true;
            # Mounts live in home directories and serve FUSE: harden the
            # rest, not the paths the drive needs.
            ProtectSystem = "yes";
            ProtectHome = "no";
            PrivateTmp = true;
          }
          // lib.optionalAttrs (drive.mode == "mount") {
            DeviceAllow = [ "/dev/fuse rw" ];
          };
        }
    )
    enabled;

  # Switch-time conflict probe (OD-34-C): the switch is refused when
  # a foreign process holds the drive. A holder that is this
  # service's own previous generation rolls the switch instead: the
  # probe compares the lock holder against the unit's main PID, so a
  # running service does not block its own upgrade — or any
  # unrelated switch while it runs.
  system.activationScripts.wyrdDrives = {
    text = lib.concatStringsSep "\n" (
      lib.mapAttrsToList
        (name: drive: ''
          mainPid=$(systemctl show -p MainPID --value 'wyrd-${name}' 2>/dev/null || echo 0)
          case "$mainPid" in ""|*[!0-9]*) mainPid=0;; esac
          PATH=${lib.makeBinPath [ pkgs.lsof ]}:/usr/bin:$PATH \
            ${probe}/bin/wyrd-drive-probe '${toString drive.driveDir}' "$mainPid" || exit 1
        '')
        enabled
    );
  };
}
