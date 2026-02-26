{
  pkgs,
  dora,
  dhcpLoadtest,
  ...
}:
let
  mkDoraConfig =
    {
      instanceId,
      serverId,
    }:
    pkgs.writeText "dora-${instanceId}.yaml" ''
      backend_mode: nats
      interfaces:
        - "eth2"
      nats:
        servers:
          - "nats://192.168.1.4:4222"
          - "nats://192.168.1.5:4222"
        subject_prefix: "dora.cluster"
        contract_version: "1.0.0"
        leases_bucket: "dora_leases"
        host_options_bucket: "dora_host_options"

      networks:
        192.168.2.0/24:
          server_id: ${serverId}
          server_name: "dora-pxe"
          file_name: "default-boot.ipxe"
          ranges:
            -
              start: 192.168.2.50
              end: 192.168.2.200
              config:
                lease_time:
                  default: 300
                  min: 60
                  max: 600
              options:
                values:
                  1:
                    type: ip
                    value: 255.255.255.0
                  3:
                    type: ip
                    value:
                      - 192.168.2.1
    '';

  mkDhcpNode =
    {
      instanceId,
      controlIp,
      dhcpIp,
      serverId,
      peerNatsIp,
    }:
    { pkgs, ... }:
    {
      virtualisation.vlans = [
        1
        2
      ];
      networking.firewall.enable = false;
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = controlIp;
          prefixLength = 24;
        }
      ];
      networking.interfaces.eth2.ipv4.addresses = [
        {
          address = dhcpIp;
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
        "d /var/lib/dora 0755 root root - -"
      ];

      environment.systemPackages = with pkgs; [
        curl
        iproute2
        jq
        nats-server
        natscli
        netcat
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
        description = "Dora DHCP Server (${instanceId})";
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
          DORA_ID = "dora-${instanceId}";
          DORA_LOG = "debug";
        };
        serviceConfig = {
          Type = "simple";
          ExecStart = "${dora}/bin/dora -c ${
            mkDoraConfig { inherit instanceId serverId; }
          } -d /var/lib/dora/leases-${instanceId}.db";
          WorkingDirectory = "/var/lib/dora";
          Restart = "on-failure";
          RestartSec = "2s";
        };
      };
    };
in
pkgs.testers.nixosTest {
  name = "dhcp-nats-jetstream-load";

  nodes = {
    dhcp1 = mkDhcpNode {
      instanceId = "1";
      controlIp = "192.168.1.4";
      dhcpIp = "192.168.2.2";
      serverId = "192.168.2.2";
      peerNatsIp = "192.168.1.5";
    };

    dhcp2 = mkDhcpNode {
      instanceId = "2";
      controlIp = "192.168.1.5";
      dhcpIp = "192.168.2.3";
      serverId = "192.168.2.3";
      peerNatsIp = "192.168.1.4";
    };

    client =
      { pkgs, ... }:
      {
        virtualisation.vlans = [ 2 ];
        networking.firewall.enable = false;

        networking.interfaces.eth1.ipv4.addresses = [
          {
            address = "192.168.2.10";
            prefixLength = 24;
          }
        ];
        networking.interfaces.eth1.macAddress = "02:00:00:00:10:01";

        environment.systemPackages = with pkgs; [
          dhcpLoadtest
          iproute2
          jq
          kea
        ];
      };
  };

  testScript = ''
    import time

    HOST_BUCKET = "dora_host_options"
    DEFAULT_BOOT_FILE = "default-boot.ipxe"
    HOST_BOOT_FILE = "host-special.ipxe"

    def sanitize_mac(mac):
      return mac.lower().replace(":", "_")

    def wait_stack_ready():
      dhcp1.wait_for_unit("nats.service")
      dhcp2.wait_for_unit("nats.service")
      dhcp1.wait_for_open_port(4222)
      dhcp2.wait_for_open_port(4222)

      dhcp1.wait_for_unit("dora.service")
      dhcp2.wait_for_unit("dora.service")
      dhcp1.wait_for_open_port(67)
      dhcp2.wait_for_open_port(67)

      dhcp1.wait_until_succeeds("nats --server nats://127.0.0.1:4222 account info >/dev/null 2>&1")
      dhcp2.wait_until_succeeds("nats --server nats://127.0.0.1:4222 account info >/dev/null 2>&1")

      dhcp1.wait_until_succeeds(
        "nats --server nats://127.0.0.1:4222 kv info dora_host_options >/dev/null 2>&1 || nats --server nats://127.0.0.1:4222 kv add dora_host_options >/dev/null 2>&1"
      )
      client.succeed("ip link set eth1 up")

    def run_loadtest(server, out_path, extra_args=""):
      client.succeed(
        f"""
        dhcp-loadtest \\
          --iface eth1 \\
          --protocol v4 \\
          --server-v4 {server}:67 \\
          --clients 20 \\
          --concurrency 8 \\
          --ramp-per-sec 15 \\
          --timeout-ms 1500 \\
          --retries 2 \\
          --renew \\
          --release \\
          --max-error-rate 0.05 \\
          --json \\
          {extra_args} > {out_path}
        """
      )
      client.succeed(f"jq -e '.passed == true and .totals.v4_failures == 0' {out_path} >/dev/null")

    def run_single_probe(server, seed, out_path):
      client.succeed(
        f"""
        dhcp-loadtest \\
          --iface eth1 \\
          --protocol v4 \\
          --server-v4 {server}:67 \\
          --clients 1 \\
          --concurrency 1 \\
          --ramp-per-sec 1 \\
          --timeout-ms 1500 \\
          --retries 2 \\
          --release \\
          --seed {seed} \\
          --json > {out_path}
        """
      )
      client.succeed(f"jq -e '.passed == true and .totals.v4_success == 1' {out_path} >/dev/null")

    def perfdhcp_check(server, log_path):
      client.succeed(
        f"""
        perfdhcp \\
          -4 \\
          -l 192.168.2.10 \\
          -r 15 \\
          -R 40 \\
          -n 40 \\
          -D 0 \\
          -u \\
          {server} > {log_path} 2>&1
        """
      )

    start_all()

    with subtest("NATS JetStream + clustered DHCP are ready"):
      wait_stack_ready()
      dhcp1.wait_until_succeeds("journalctl -u dora.service --no-pager -o cat | grep -q 'NATS connection established for nats mode'")
      dhcp2.wait_until_succeeds("journalctl -u dora.service --no-pager -o cat | grep -q 'NATS connection established for nats mode'")

    with subtest("Host-option override returns expected boot image"):
      seed = 4242
      client.succeed(
        f"dhcp-loadtest --iface eth1 --protocol v4 --server-v4 192.168.2.2:67 --clients 1 --seed {seed} --dry-run --json > /tmp/identity.json"
      )
      mac = client.succeed("jq -r '.clients[0].mac' /tmp/identity.json").strip()
      key = f"v4/mac/{sanitize_mac(mac)}"

      dhcp1.succeed(
        f"nats --server nats://127.0.0.1:4222 kv put {HOST_BUCKET} {key} '{{\"boot_file\":\"{HOST_BOOT_FILE}\",\"next_server\":\"10.0.0.42\"}}'"
      )
      run_single_probe("192.168.2.2", seed, "/tmp/host-override.json")
      boot_file = client.succeed("jq -r '.clients[0].v4.boot_file // \"\"' /tmp/host-override.json").strip()
      assert boot_file == HOST_BOOT_FILE, f"expected host override boot file {HOST_BOOT_FILE}, got {boot_file}"

    with subtest("Removing host-option key reverts to default boot image"):
      seed = 4242
      mac = client.succeed("jq -r '.clients[0].mac' /tmp/identity.json").strip()
      key = f"v4/mac/{sanitize_mac(mac)}"

      dhcp1.succeed(f"nats --server nats://127.0.0.1:4222 kv del {HOST_BUCKET} {key} >/dev/null 2>&1 || true")
      time.sleep(1)
      run_single_probe("192.168.2.2", seed, "/tmp/host-default.json")
      boot_file = client.succeed("jq -r '.clients[0].v4.boot_file // \"\"' /tmp/host-default.json").strip()
      assert boot_file == DEFAULT_BOOT_FILE, f"expected default boot file {DEFAULT_BOOT_FILE}, got {boot_file}"

    with subtest("dhcp-loadtest validates both DHCP servers"):
      run_loadtest("192.168.2.2", "/tmp/load-dhcp1.json", "--seed 11")
      run_loadtest("192.168.2.3", "/tmp/load-dhcp2.json", "--seed 12")

    with subtest("perfdhcp load and uniqueness checks on both servers"):
      perfdhcp_check("192.168.2.2", "/tmp/perfdhcp-dhcp1.log")
      perfdhcp_check("192.168.2.3", "/tmp/perfdhcp-dhcp2.log")

    with subtest("Final service health"):
      dhcp1.succeed("systemctl is-active dora.service")
      dhcp2.succeed("systemctl is-active dora.service")
      dhcp1.succeed("systemctl is-active nats.service")
      dhcp2.succeed("systemctl is-active nats.service")
  '';
}
