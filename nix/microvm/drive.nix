# nix/microvm/drive.nix — drive-service evidence guest: one vault and
# one mount declared through services.wyrd.drives. Static network on
# tap-d (10.0.7.13/24, next free slot after the e2e trio). Values must
# match [tasks.wyrd-drive-e2e.topology.box.drive] in gate.toml (pinned
# by tests/harness/test-topology-toml.sh).
{ lib, ... }:
{
  networking.hostName = "wyrd-drive";
  networking.interfaces.eth0.ipv4.addresses =
    [{ address = "10.0.7.13"; prefixLength = 24; }];

  # TEMP-DIAG: the first boot of this guest hangs pre-network with a
  # silent console (the microvm base appends loglevel=4 after our
  # params, so a plain value loses). mkAfter puts ours last, where
  # the kernel honors them. Revert once the hang point is found.
  boot.kernelParams = lib.mkAfter [ "loglevel=7" "systemd.log_level=debug" ];

  microvm = {
    vcpu = 2;
    mem = 2048;
    interfaces = [{
      type = "tap";
      id = "tap-d";
      mac = "02:00:00:00:07:13";
    }];
  };
}
