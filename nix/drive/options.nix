# Shared declarations for the declarative Wyrd drive services
# (OD-34-A, option A): one module, two modes, one declaration per
# drive. This file holds the option surface and the
# platform-independent assertions; the NixOS and nix-darwin files
# import it and add their supervisor shape. One file cannot serve
# both systems directly: a key the evaluating system never declares
# (`launchd` on NixOS, `systemd` on nix-darwin) fails the
# unmatched-option check even under a false conditional, so the
# foreign keys must be absent, not merely disabled.
{ config, lib, ... }:

let
  cfg = config.services.wyrd;
  enabled = lib.filterAttrs (_: drive: drive.enable) cfg.drives;
in
{
  options.services.wyrd.drives = lib.mkOption {
    type = lib.types.attrsOf (lib.types.submodule (
      { name, ... }:
      {
        options = {
          enable = lib.mkEnableOption "the Wyrd drive service ${name}";
          driveDir = lib.mkOption {
            type = lib.types.path;
            description = "Drive directory this service owns, exclusively.";
          };
          mode = lib.mkOption {
            type = lib.types.enum [
              "vault"
              "mount"
            ];
            description = "Supervised mode: headless owner, or FUSE session.";
          };
          mountpoint = lib.mkOption {
            type = lib.types.nullOr lib.types.path;
            default = null;
            description = "FUSE mountpoint. Required in mount mode, refused in vault mode.";
          };
          identityFile = lib.mkOption {
            type = lib.types.path;
            description = "Device identity key file. A path, never a store string.";
          };
          passphraseFile = lib.mkOption {
            type = lib.types.nullOr lib.types.path;
            default = null;
            description = "Keystore passphrase file. A path, never a store string.";
          };
          relays = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [ ];
            description = "Relay URLs the drive syncs through.";
          };
          logFile = lib.mkOption {
            type = lib.types.nullOr lib.types.path;
            default = null;
            description = "Operator-chosen vault log file. Vault mode only; the mount logs to its drive directory.";
          };
          package = lib.mkOption {
            type = lib.types.package;
            description = "The wyrd package to supervise (the flake's packages.wyrd).";
          };
        };
      }
    ));
    default = { };
    description = "Declarative Wyrd drive services, one per drive directory.";
  };

  config = {
    assertions =
      let
        driveDirs = lib.mapAttrsToList (_: drive: toString drive.driveDir) enabled;
        duplicateDirs = lib.filter
          (
            dir: lib.length (lib.filter (other: other == dir) driveDirs) > 1
          )
          (lib.unique driveDirs);
        secretOf = drive:
          [ (toString drive.identityFile) ]
          ++ lib.optionals (drive.passphraseFile != null) [ (toString drive.passphraseFile) ]
          ++ lib.optionals (drive.logFile != null) [ (toString drive.logFile) ];
        storeSecrets = lib.filter (path: lib.hasPrefix "/nix/store" path) (
          lib.concatMap secretOf (lib.attrValues enabled)
        );
      in
      [
        {
          assertion = duplicateDirs == [ ];
          message = "services.wyrd.drives declares one drive twice: ${lib.concatStringsSep ", " duplicateDirs}. One owner process per drive directory.";
        }
        {
          assertion = storeSecrets == [ ];
          message = "services.wyrd.drives puts a secret in the Nix store: ${lib.concatStringsSep ", " storeSecrets}. Secrets are paths, never store strings.";
        }
      ]
      ++ lib.mapAttrsToList
        (name: drive: {
          assertion = drive.mode != "mount" || drive.mountpoint != null;
          message = "services.wyrd.drives.${name}: mount mode needs a mountpoint.";
        })
        enabled
      ++ lib.mapAttrsToList
        (name: drive: {
          assertion = drive.mode != "vault" || drive.mountpoint == null;
          message = "services.wyrd.drives.${name}: a vault mounts nothing; mountpoint belongs to mount mode.";
        })
        enabled
      ++ lib.mapAttrsToList
        (name: drive: {
          assertion = drive.mode != "mount" || drive.logFile == null;
          message = "services.wyrd.drives.${name}: the mount logs to its drive directory; logFile belongs to vault mode.";
        })
        enabled;
  };
}
