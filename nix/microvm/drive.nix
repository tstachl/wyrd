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
    # Gate-owned DHCP client; the lease duplicates the static ip
    # given the host reservation. Explicit per guest (the gate
    # default is also on): intent, immune to default flips.
    dhcp = true;
    vcpu = 2;
    memMb = 2048;
    mac = "02:00:00:00:07:13";
    ip = "10.0.7.13";
  };

  # FUSE device node: the mount-mode unit mounts FUSE and the module
  # only allowlists the device (DeviceAllow) — nothing loads the
  # driver, and fuser opens /dev/fuse directly (no mount-syscall
  # autoload). The microvm guest had it; the Incus image needs the
  # module listed or the mount fails ENOENT and the unit burns its
  # start budget. Scoped here, not in common.nix: only the drive
  # guest serves FUSE. (Follow-up: the module should declare its own
  # fuse precondition instead of relying on the evidence guest.)
  boot.kernelModules = [ "fuse" ];
}
