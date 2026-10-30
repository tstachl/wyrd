# nix/microvm/drive.nix — drive-service evidence guest: one vault and
# one mount declared through services.wyrd.drives. Static network on
# tap-d (10.0.7.13/24, next free slot after the e2e trio). Values must
# match [tasks.wyrd-drive-e2e.topology.box.drive] in gate.toml (pinned
# by tests/harness/test-topology-toml.sh).
{ ... }:
{
  networking.hostName = "wyrd-drive";
  networking.interfaces.eth0.ipv4.addresses =
    [{ address = "10.0.7.13"; prefixLength = 24; }];

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
