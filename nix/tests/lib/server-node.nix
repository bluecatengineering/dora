# Shared server VM node builders for dora DHCP integration tests.
#
# mkStandaloneNode:  single dora server in standalone (SQLite) mode.
# mkNatsNode:        dora server + NATS server for clustered mode.
{
  pkgs,
  dora,
  doraConfigs,
}:

let
  # Common base configuration shared by all server nodes.
  mkServerBase =
    {
      dhcpIp,
      dhcpV6,
      vlans,
      dhcpIface,
    }:
    { ... }:
    {
      virtualisation.vlans = vlans;
      networking.firewall.enable = false;

      networking.interfaces.${dhcpIface} = {
        ipv4.addresses = [
          {
            address = dhcpIp;
            prefixLength = 24;
          }
        ];
        ipv6.addresses = [
          {
            address = dhcpV6;
            prefixLength = 64;
          }
        ];
      };

      systemd.tmpfiles.rules = [
        "d /var/lib/dora 0755 root root - -"
      ];

      environment.systemPackages = with pkgs; [
        curl
        iproute2
        jq
        netcat
      ];
    };
in
{
  # Create a standalone dora server node (no NATS).
  # Uses a single VLAN and SQLite-backed leases.
  mkStandaloneNode =
    {
      instanceId ? "1",
      dhcpIp ? "192.168.2.2",
      dhcpV6 ? "fd00:2::2",
      serverId ? "192.168.2.2",
    }:
    {
      pkgs,
      lib,
      ...
    }:
    lib.mkMerge [
      (mkServerBase {
        inherit dhcpIp dhcpV6;
        vlans = [ 2 ];
        dhcpIface = "eth1";
      } { })
      {
        systemd.services.dora = {
          description = "Dora DHCP Server (standalone-${instanceId})";
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          wantedBy = [ "multi-user.target" ];
          environment = {
            DORA_ID = "dora-standalone-${instanceId}";
            DORA_LOG = "debug";
          };
          serviceConfig = {
            Type = "simple";
            ExecStart = "${dora}/bin/dora -c ${
              doraConfigs.mkStandaloneConfig {
                inherit instanceId serverId;
              }
            } -d /var/lib/dora/leases-${instanceId}.db";
            WorkingDirectory = "/var/lib/dora";
            Restart = "on-failure";
            RestartSec = "2s";
          };
        };
      }
    ];

  # Create a NATS-clustered dora server node (NATS + dora).
  # Uses two VLANs: VLAN 1 for NATS clustering, VLAN 2 for DHCP service.
  mkNatsNode =
    {
      instanceId,
      controlIp,
      dhcpIp,
      dhcpV6,
      serverId,
      peerNatsIp,
    }:
    {
      pkgs,
      lib,
      ...
    }:
    lib.mkMerge [
      (mkServerBase {
        inherit dhcpIp dhcpV6;
        vlans = [
          1
          2
        ];
        dhcpIface = "eth2";
      } { })
      {
        networking.interfaces.eth1.ipv4.addresses = [
          {
            address = controlIp;
            prefixLength = 24;
          }
        ];

        users.groups.nats = { };
        users.users.nats = {
          isSystemUser = true;
          group = "nats";
        };

        systemd.tmpfiles.rules = [
          "d /var/lib/nats 0755 nats nats - -"
        ];

        environment.systemPackages = with pkgs; [
          nats-server
          natscli
        ];

        systemd.services.nats = {
          description = "NATS Server (dhcp-${instanceId})";
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          wantedBy = [ "multi-user.target" ];
          serviceConfig = {
            Type = "simple";
            User = "nats";
            Group = "nats";
            Restart = "on-failure";
            RestartSec = "2s";
            ExecStart = ''
              ${pkgs.nats-server}/bin/nats-server \
                -a 0.0.0.0 \
                -p 4222 \
                -js \
                -sd /var/lib/nats \
                -n dora-nats-${instanceId} \
                --cluster_name dora-js \
                --cluster nats://0.0.0.0:6222 \
                --routes nats://${peerNatsIp}:6222
            '';
          };
        };

        systemd.services.dora = {
          description = "Dora DHCP Server (nats-${instanceId})";
          after = [
            "network-online.target"
            "nats.service"
          ];
          wants = [
            "network-online.target"
            "nats.service"
          ];
          wantedBy = [ "multi-user.target" ];
          environment = {
            DORA_ID = "dora-nats-${instanceId}";
            DORA_LOG = "debug";
          };
          serviceConfig = {
            Type = "simple";
            ExecStart = "${dora}/bin/dora -c ${
              doraConfigs.mkNatsConfig {
                inherit instanceId serverId;
              }
            } -d /var/lib/dora/leases-${instanceId}.db";
            WorkingDirectory = "/var/lib/dora";
            Restart = "on-failure";
            RestartSec = "2s";
          };
        };
      }
    ];
}
