# nix/microvm/peer-n.nix — member peer: joins via invitation, converges
# over the relay, exercises fetch-on-open demand across hosts.
# Shape and addressing for the gate.image/v1 manifest, the guest
# network config, and the image metadata derive from gate.box (see
# nix/microvm/common.nix and image.nix): this file is the single
# source, so the three can never drift.
{ ... }:
{
  gate.box = {
    role = "peer-n";
    vcpu = 4;
    memMb = 4096;
    mac = "02:00:00:00:07:12";
    ip = "10.0.7.12";
  };
}
