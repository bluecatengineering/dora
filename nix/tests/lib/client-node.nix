# Shared client VM node builder for dora DHCP integration tests.
#
# mkMatrixClientNode builds a client VM with all the DHCP client tools
# needed for the compatibility matrix test.
{ pkgs, dhcpLoadtest }:

let
  commonClientPackages =
    extraPackages:
    with pkgs;
    [
      # Core utilities
      iproute2
      jq
      gawk
      gnugrep
      procps

      # DHCP clients under test
      dhcpcd # Full-featured DHCP client daemon
      busybox # Provides udhcpc (lightweight/embedded)
      kea # Provides perfdhcp (load testing)
      dhcpm # Rust DHCP message CLI
      dhcping # DHCP server ping/probe

      # Our custom load test tool
      dhcpLoadtest
    ]
    ++ extraPackages;

  mkClientBase =
    {
      clientIp,
      clientV6,
      clientMac,
      extraPackages,
    }:
    { pkgs, ... }:
    {
      virtualisation.vlans = [ 2 ];
      networking.firewall.enable = false;

      # Give the client a static address for tool-based testing
      # (perfdhcp, dhcpm, etc.).  Real DHCP clients will flush this
      # and obtain their own.
      networking.interfaces.eth1 = {
        ipv4.addresses = [
          {
            address = clientIp;
            prefixLength = 24;
          }
        ];
        ipv6.addresses = [
          {
            address = clientV6;
            prefixLength = 64;
          }
        ];
        macAddress = clientMac;
      };

      # Disable the default NixOS DHCP client so it doesn't interfere
      networking.useDHCP = false;

      # Enable systemd-networkd so the systemd-networkd client test can use it.
      # It starts idle (no .network files matching our test interface).
      systemd.services.systemd-networkd.wantedBy = pkgs.lib.mkForce [ "multi-user.target" ];
      networking.useNetworkd = pkgs.lib.mkForce true;

      environment.systemPackages = commonClientPackages extraPackages;
    };
in
{
  mkMatrixClientNode =
    {
      clientIp ? "192.168.2.10",
      clientV6 ? "fd00:2::10",
      clientMac ? "02:00:00:00:10:01",
      extraPackages ? [ ],
    }:
    mkClientBase {
      inherit
        clientIp
        clientV6
        clientMac
        extraPackages
        ;
    };

  mkNatsClientNode =
    {
      clientIp ? "192.168.2.10",
      clientV6 ? "fd00:2::10",
      clientMac ? "02:00:00:00:10:01",
      extraPackages ? [ ],
    }:
    mkClientBase {
      inherit
        clientIp
        clientV6
        clientMac
        extraPackages
        ;
    };
}
