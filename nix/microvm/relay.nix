# nix/microvm/relay.nix — control-plane relay as a VM service, always
# up for the run. The suite never manages it (RELAY_MANAGED=0): peer
# restarts are the test, relay restarts are not.
#
# Nothing ever logs into the relay: it serves the control plane
# only. (There is no ssh and no share; the Incus agent is the only
# control plane, and the suite addresses the relay's port, never a
# shell there.)
#
# Shape and addressing for the gate.image/v1 manifest, the guest
# network config, and the image metadata derive from gate.box (see
# the gate input's nixosModules.guest): this file is the single
# source, so the three can never drift.
{ pkgs, ... }:
{
  gate.box = {
    role = "relay";
    # Gate-owned DHCP client (option lands with the gate PR; parked until then).
    dhcp = true;
    vcpu = 2;
    memMb = 1024;
    mac = "02:00:00:00:07:10";
    ip = "10.0.7.10";
  };

  environment.etc."wyrd-relay.toml".text = ''
    [info]
    relay_url = "ws://10.0.7.10:18761/"
    name = "wyrd-microvm"
    description = "microVM e2e relay; one run."

    [network]
    address = "0.0.0.0"
    port = 18761
  '';

  systemd.services.nostr-relay = {
    wantedBy = [ "multi-user.target" ];
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
    serviceConfig = {
      ExecStart = "${pkgs.nostr-rs-relay}/bin/nostr-rs-relay -c /etc/wyrd-relay.toml -d /var/lib/wyrd-relay";
      Restart = "on-failure";
      StateDirectory = "wyrd-relay";
    };
  };

  # Post-mortem forensics ride the host now: the provisioner captures
  # the instance console log and instance info at teardown, and the
  # suite pulls guest logs on demand over the agent. No guest-side
  # journal dumper: with no shared filesystem there is nowhere for a
  # guest timer to publish to, and a guest-local dump the suite
  # never pulls is write-only forensics.
}
