# nix/microvm/peer-o.nix — owner peer: first writer, drive author.
# Shape and addressing for the gate.image/v1 manifest, the guest
# network config, and the image metadata derive from gate.box (see
# the gate input's nixosModules.guest): this file is the single
# source, so the three can never drift.
{ ... }:
{
  gate.box = {
    role = "peer-o";
    # Gate-owned DHCP client; the lease duplicates the static ip
    # given the host reservation. Explicit per guest (the gate
    # default is also on): intent, immune to default flips.
    dhcp = true;
    vcpu = 4;
    memMb = 4096;
    mac = "02:00:00:00:07:11";
    ip = "10.0.7.11";
  };
}
