# Shared dora DHCP server configuration generators.
#
# mkDoraConfig generates a YAML config file for either standalone or NATS mode.
# Both modes share the same network/range/option definitions; the only
# difference is the backend_mode and optional NATS stanza.
{ pkgs }:

{
  # Generate a dora config file for standalone (SQLite) mode.
  mkStandaloneConfig =
    {
      instanceId,
      serverId,
      interfaces ? [ "eth1" ],
      v6Interfaces ? [ "eth1" ],
      leaseTime ? 300,
      rangeStart ? "192.168.2.50",
      rangeEnd ? "192.168.2.200",
      subnetMask ? "255.255.255.0",
      router ? "192.168.2.1",
      serverName ? "dora-pxe",
      fileName ? "default-boot.ipxe",
      v6Prefix ? "fd00:2::/64",
      v6Dns ? "fd00:2::1",
    }:
    pkgs.writeText "dora-standalone-${instanceId}.yaml" ''
      interfaces:
      ${pkgs.lib.concatMapStringsSep "\n" (i: "  - \"${i}\"") interfaces}

      networks:
        192.168.2.0/24:
          server_id: ${serverId}
          server_name: "${serverName}"
          file_name: "${fileName}"
          ranges:
            -
              start: ${rangeStart}
              end: ${rangeEnd}
              config:
                lease_time:
                  default: ${toString leaseTime}
                  min: 60
                  max: 600
              options:
                values:
                  1:
                    type: ip
                    value: ${subnetMask}
                  3:
                    type: ip
                    value:
                      - ${router}
                  6:
                    type: ip
                    value:
                      - ${router}

      v6:
        interfaces:
        ${pkgs.lib.concatMapStringsSep "\n" (i: "  - \"${i}\"") v6Interfaces}
        server_id:
          type: LLT
          persist: false
        options:
          values:
            23:
              type: ip_list
              value:
                - ${v6Dns}
        networks:
          ${v6Prefix}:
            interfaces:
            ${pkgs.lib.concatMapStringsSep "\n" (i: "  - \"${i}\"") v6Interfaces}
            config:
              lease_time:
                default: ${toString leaseTime}
              preferred_time:
                default: ${toString (leaseTime / 2)}
            options:
              values:
                23:
                  type: ip_list
                  value:
                    - ${v6Dns}
    '';

  # Generate a dora config file for NATS-clustered mode.
  mkNatsConfig =
    {
      instanceId,
      serverId,
      natsServers ? [
        "nats://192.168.1.4:4222"
        "nats://192.168.1.5:4222"
      ],
      interfaces ? [ "eth2" ],
      v6Interfaces ? [ "eth2" ],
      leaseTime ? 300,
      rangeStart ? "192.168.2.50",
      rangeEnd ? "192.168.2.200",
      subnetMask ? "255.255.255.0",
      router ? "192.168.2.1",
      serverName ? "dora-pxe",
      fileName ? "default-boot.ipxe",
      v6Prefix ? "fd00:2::/64",
      v6Dns ? "fd00:2::1",
    }:
    pkgs.writeText "dora-nats-${instanceId}.yaml" ''
      backend_mode: nats
      interfaces:
      ${pkgs.lib.concatMapStringsSep "\n" (i: "  - \"${i}\"") interfaces}
      nats:
        servers:
      ${pkgs.lib.concatMapStringsSep "\n" (s: "    - \"${s}\"") natsServers}
        subject_prefix: "dora.cluster"
        contract_version: "1.0.0"
        leases_bucket: "dora_leases"
        host_options_bucket: "dora_host_options"

      networks:
        192.168.2.0/24:
          server_id: ${serverId}
          server_name: "${serverName}"
          file_name: "${fileName}"
          ranges:
            -
              start: ${rangeStart}
              end: ${rangeEnd}
              config:
                lease_time:
                  default: ${toString leaseTime}
                  min: 60
                  max: 600
              options:
                values:
                  1:
                    type: ip
                    value: ${subnetMask}
                  3:
                    type: ip
                    value:
                      - ${router}
                  6:
                    type: ip
                    value:
                      - ${router}

      v6:
        interfaces:
        ${pkgs.lib.concatMapStringsSep "\n" (i: "  - \"${i}\"") v6Interfaces}
        server_id:
          type: LLT
          persist: false
        options:
          values:
            23:
              type: ip_list
              value:
                - ${v6Dns}
        networks:
          ${v6Prefix}:
            interfaces:
            ${pkgs.lib.concatMapStringsSep "\n" (i: "  - \"${i}\"") v6Interfaces}
            config:
              lease_time:
                default: ${toString leaseTime}
              preferred_time:
                default: ${toString (leaseTime / 2)}
            options:
              values:
                23:
                  type: ip_list
                  value:
                    - ${v6Dns}
    '';
}
