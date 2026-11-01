# nix/microvm/drive.nix — drive-service evidence guest: one vault and
# one mount declared through services.wyrd.drives (composed in
# flake.nix via extraModules). Shape and addressing for the
# gate.image/v1 manifest derive from gate.box (see the gate input's
# nixosModules.guest): this file is the single source, like
# peer-o.nix for the trio. The box rides the next free slot after
# the trio (.13); mem is the plain 2 GiB — the old 2049 was a
# microvm-machine ACPI-table workaround, meaningless on Incus.
{ ... }:
{
  gate.box = {
    role = "drive";
    vcpu = 2;
    memMb = 2048;
    mac = "02:00:00:00:07:13";
    ip = "10.0.7.13";
  };
}
