# nix-darwin half of the declarative Wyrd drive services: launchd
# daemons plus the switch-time conflict probe. Imports the shared
# option surface; the NixOS half lives beside this file. Secrets are
# paths, never store strings (asserted in the options file).
#
# A system daemon, not a login-session agent: the drive starts at
# boot and restarts with nobody logged in. macFUSE is a manual
# prerequisite the module cannot install; the mount's preflight
# fails with the actionable message when the kext is missing.
{ config, lib, pkgs, ... }:

let
  enabled = lib.filterAttrs (_: drive: drive.enable) config.services.wyrd.drives;

  supervise = pkgs.writers.writePython3Bin "wyrd-supervise" { } (
    builtins.readFile ./supervise.py
  );
  probe = pkgs.writers.writePython3Bin "wyrd-drive-probe" { } (
    builtins.readFile ./probe.py
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
  # 78 is terminal (never restarted); launchd's KeepAlive with
  # SuccessfulExit restarts on zero and not otherwise, so the
  # restart-worthy status is 0.
  wrapperArgv = drive:
    [ "${supervise}/bin/wyrd-supervise" "--restart-status=0" "--posture-path=${posturePath drive}" "--" ]
    ++ wyrdArgs drive;
in
{
  imports = [ ./options.nix ];

  launchd.daemons = lib.mapAttrs'
    (
      name: drive:
        lib.nameValuePair "md.enesis.wyrd.${name}" {
          serviceConfig = {
            Label = "md.enesis.wyrd.${name}";
            ProgramArguments = wrapperArgv drive;
            RunAtLoad = true;
            # Restart on the wrapper's exit 0 (signal death, runtime
            # error), never on its exit 78 (terminal: held drive).
            KeepAlive = {
              SuccessfulExit = true;
            };
            # Spaces restarts; the count is bounded by the terminal
            # status, not by this interval.
            ThrottleInterval = 30;
            StandardOutPath = "/var/log/wyrd-${name}.log";
            StandardErrorPath = "/var/log/wyrd-${name}.log";
            WorkingDirectory = "/";
          };
        }
    )
    enabled;

  # Switch-time conflict probe (OD-34-C): same contract as the NixOS
  # half — refuse the switch on a foreign holder, roll it when the
  # holder is this service's own previous generation (compared
  # against the daemon's PID from launchctl).
  system.activationScripts.wyrdDrives = {
    text = lib.concatStringsSep "\n" (
      lib.mapAttrsToList
        (name: drive: ''
          mainPid=$(launchctl list 'md.enesis.wyrd.${name}' 2>/dev/null | awk 'NR==1 {print $1}')
          case "$mainPid" in ""|*[!0-9]*) mainPid=0;; esac
          PATH=${lib.makeBinPath [ pkgs.lsof pkgs.gawk ]}:/usr/sbin:$PATH \
            ${probe}/bin/wyrd-drive-probe '${toString drive.driveDir}' "$mainPid" || exit 1
        '')
        enabled
    );
  };
}
