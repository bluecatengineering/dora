{
  pkgs,
  dora,
  dhcpLoadtest,
  ...
}:
let
  # Import the shared test library for node builders and config generators.
  testLib = import ./lib {
    inherit pkgs dora dhcpLoadtest;
  };
in
pkgs.testers.nixosTest {
  name = "dhcp-nats-jetstream-load";

  nodes = {
    dhcp1 = testLib.mkNatsNode {
      instanceId = "1";
      controlIp = "192.168.1.4";
      dhcpIp = "192.168.2.2";
      dhcpV6 = "fd00:2::2";
      serverId = "192.168.2.2";
      peerNatsIp = "192.168.1.5";
    };

    dhcp2 = testLib.mkNatsNode {
      instanceId = "2";
      controlIp = "192.168.1.5";
      dhcpIp = "192.168.2.3";
      dhcpV6 = "fd00:2::3";
      serverId = "192.168.2.3";
      peerNatsIp = "192.168.1.4";
    };

    client = testLib.mkNatsClientNode {
      clientIp = "192.168.2.10";
      clientV6 = "fd00:2::10";
      clientMac = "02:00:00:00:10:01";
    };
  };

  testScript = ''
    # Import shared helpers
    ${testLib.testHelpers}

    HOST_BUCKET = "dora_host_options"
    DEFAULT_BOOT_FILE = "default-boot.ipxe"
    HOST_BOOT_FILE = "host-special.ipxe"
    IFACE = "eth1"

    def sanitize_mac(mac):
      return mac.lower().replace(":", "_")

    def run_loadtest(server, out_path, extra_args=""):
      client.succeed(
        f"""
        dhcp-loadtest \
          --iface {IFACE} \
          --protocol v4 \
          --server-v4 {server}:67 \
          --clients 20 \
          --concurrency 8 \
          --ramp-per-sec 15 \
          --timeout-ms 2500 \
          --retries 3 \
          --release \
          --max-error-rate 0.05 \
          --json \
          {extra_args} > {out_path}
        """
      )
      client.succeed(f"jq -e '.passed == true and .totals.v4_failures == 0' {out_path} >/dev/null")

    def run_single_probe(server, seed, out_path):
      client.succeed(
        f"""
        dhcp-loadtest \
          --iface {IFACE} \
          --protocol v4 \
          --server-v4 {server}:67 \
          --clients 1 \
          --concurrency 1 \
          --ramp-per-sec 1 \
          --timeout-ms 1500 \
          --retries 2 \
          --release \
          --seed {seed} \
          --json > {out_path}
        """
      )
      client.succeed(f"jq -e '.passed == true and .totals.v4_success == 1' {out_path} >/dev/null")

    def run_loadtest_v6(server, out_path, extra_args=""):
      client.succeed(
        f"""
        dhcp-loadtest \
          --iface {IFACE} \
          --protocol v6 \
          --server-v6 [{server}]:547 \
          --clients 12 \
          --concurrency 4 \
          --ramp-per-sec 8 \
          --timeout-ms 2500 \
          --retries 3 \
          --release \
          --max-error-rate 0.05 \
          --json \
          {extra_args} > {out_path}
        """
      )
      client.succeed(
        f"jq -e '.passed == true and .totals.v6_failures == 0 and .totals.v6_success == 12' {out_path} >/dev/null"
      )

    def run_single_probe_v6(server, seed, out_path):
      client.succeed(
        f"""
        dhcp-loadtest \
          --iface {IFACE} \
          --protocol v6 \
          --server-v6 [{server}]:547 \
          --clients 1 \
          --concurrency 1 \
          --ramp-per-sec 1 \
          --timeout-ms 2000 \
          --retries 3 \
          --renew \
          --release \
          --seed {seed} \
          --json > {out_path}
        """
      )
      client.succeed(
        f"jq -e '.passed == true and .totals.v6_success == 1 and .clients[0].v6.leased_ip != null and .clients[0].v6.renew_ip != null and .clients[0].v6.released == true' {out_path} >/dev/null"
      )

    def perfdhcp_check(server, log_path):
      client.succeed(
        f"""
        perfdhcp \
          -4 \
          -l 192.168.2.10 \
          -r 15 \
          -R 40 \
          -n 40 \
          -D 0 \
          -u \
          {server} > {log_path} 2>&1 || true
        """
      )
      client.succeed(f"grep -Eiq 'sent|received|drops|responses' {log_path}")

    start_all()

    with subtest("NATS JetStream + clustered DHCP are ready"):
      wait_nats_cluster_ready(dhcp1, dhcp2, client)
      dhcp1.wait_until_succeeds("journalctl -u dora.service --no-pager -o cat | grep -q 'NATS connection established for nats mode'")
      dhcp2.wait_until_succeeds("journalctl -u dora.service --no-pager -o cat | grep -q 'NATS connection established for nats mode'")

    with subtest("Host-option override returns expected boot image"):
      seed = 4242
      client.succeed(
        f"dhcp-loadtest --iface {IFACE} --protocol v4 --server-v4 192.168.2.2:67 --clients 1 --seed {seed} --dry-run --json > /tmp/identity.json"
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

      dhcp1.succeed(f"nats --server nats://127.0.0.1:4222 kv del --force {HOST_BUCKET} {key}")
      dhcp1.wait_until_succeeds(
        f"! nats --server nats://127.0.0.1:4222 kv get {HOST_BUCKET} {key} >/dev/null 2>&1"
      )
      time.sleep(1)
      run_single_probe("192.168.2.2", seed, "/tmp/host-default.json")
      boot_file = client.succeed("jq -r '.clients[0].v4.boot_file // \"\"' /tmp/host-default.json").strip()
      assert boot_file == DEFAULT_BOOT_FILE, f"expected default boot file {DEFAULT_BOOT_FILE}, got {boot_file}"

    with subtest("dhcp-loadtest validates both DHCP servers"):
      run_loadtest("192.168.2.2", "/tmp/load-dhcp1.json", "--seed 11")
      run_loadtest("192.168.2.3", "/tmp/load-dhcp2.json", "--seed 12")

    with subtest("dhcp-loadtest validates DHCPv6 on both servers"):
      run_single_probe_v6("fd00:2::2", 101, "/tmp/v6-single-dhcp1.json")
      run_single_probe_v6("fd00:2::3", 102, "/tmp/v6-single-dhcp2.json")
      run_loadtest_v6("fd00:2::2", "/tmp/v6-load-dhcp1.json", "--seed 21")
      run_loadtest_v6("fd00:2::3", "/tmp/v6-load-dhcp2.json", "--seed 22")

    with subtest("perfdhcp load and uniqueness checks on both servers"):
      perfdhcp_check("192.168.2.2", "/tmp/perfdhcp-dhcp1.log")
      perfdhcp_check("192.168.2.3", "/tmp/perfdhcp-dhcp2.log")

    with subtest("NATS mode does not use local SQLite allocator"):
      dhcp1.succeed("! journalctl -u dora.service --no-pager -o cat | grep -q 'ip_manager::sqlite'")
      dhcp2.succeed("! journalctl -u dora.service --no-pager -o cat | grep -q 'ip_manager::sqlite'")

    with subtest("Final service health"):
      dhcp1.succeed("systemctl is-active dora.service")
      dhcp2.succeed("systemctl is-active dora.service")
      dhcp1.succeed("systemctl is-active nats.service")
      dhcp2.succeed("systemctl is-active nats.service")
  '';
}
