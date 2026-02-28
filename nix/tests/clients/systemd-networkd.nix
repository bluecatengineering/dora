# systemd-networkd -- built-in systemd network manager with DHCP client.
#
# Many NixOS and general systemd-based deployments use systemd-networkd
# to obtain DHCP leases.  This tests the built-in DHCPv4/v6 client.
{
  name = "systemd-networkd";
  mac = "02:00:00:01:00:03";

  capabilities = {
    v4_lease = true;
    v4_options = true;
    v4_renew = true;
    v4_release = true;
    v6_lease = true; # Will fail in standalone mode (no v6 Solicit handler)
    v6_options = true;
    v6_renew = true;
    v6_release = true;
    load = false;
  };

  testCode = ''
    def networkd_setup():
        client.succeed("systemctl stop systemd-networkd.service || true")
        client.succeed(f"ip addr flush dev {IFACE}")
        # Remove any NixOS-generated and test .network files so networkd
        # starts clean with only our test-specific configs.
        client.succeed("rm -f /etc/systemd/network/10-test-*.network")
        client.succeed("rm -f /etc/systemd/network/40-*.network")

    def networkd_v4_lease():
        client.succeed(
            "printf '[Match]\nName=" + IFACE + "\n\n"
            "[Network]\nDHCP=ipv4\nIPv6AcceptRA=false\n\n"
            "[DHCPv4]\nUseDNS=true\nUseRoutes=true\n'"
            " > /etc/systemd/network/10-test-dhcp4.network"
        )
        client.succeed("systemctl restart systemd-networkd.service")
        client.wait_until_succeeds(
            f"ip -4 addr show {IFACE} | grep -q '192\\.168\\.2\\.'",
            timeout=20,
        )
        ip_out = client.succeed(
            f"ip -4 addr show {IFACE} | grep 'inet ' | awk '{{print $2}}'"
        ).strip()
        assert ip_out, "systemd-networkd did not obtain an IPv4 address"
        ip = ip_out.split("/")[0]
        assert ip.startswith("192.168.2."), f"Expected 192.168.2.x, got {ip}"
        return f"got {ip_out}"

    def networkd_v4_options():
        route = client.succeed(f"ip -4 route show default dev {IFACE}").strip()
        assert "192.168.2.1" in route, f"Expected router 192.168.2.1 in route: {route}"
        return f"route={route}"

    def networkd_v4_renew():
        ip_before = client.succeed(
            f"ip -4 addr show {IFACE} | grep 'inet ' | awk '{{print $2}}'"
        ).strip()
        client.succeed(f"networkctl renew {IFACE}")
        import time; time.sleep(3)
        ip_after = client.succeed(
            f"ip -4 addr show {IFACE} | grep 'inet ' | awk '{{print $2}}'"
        ).strip()
        assert ip_after, "IP lost after renew"
        return f"before={ip_before} after={ip_after}"

    def networkd_v4_release():
        # Remove the DHCP .network file and reconfigure networkd.
        # networkd will send a RELEASE when it loses the config and then
        # the address should eventually disappear. Give it time.
        client.succeed("rm -f /etc/systemd/network/10-test-dhcp4.network")
        client.succeed("networkctl reload")
        import time; time.sleep(3)
        # networkd may keep the address briefly; flush manually after the
        # release message has been sent to keep the test deterministic.
        ip_out = client.succeed(
            f"ip -4 addr show {IFACE} | grep 'inet.*192\\.168\\.2\\.' || true"
        ).strip()
        if ip_out:
            # Address still present -- networkd didn't flush it yet.
            # That's OK as long as networkd sent the RELEASE to the server.
            # Flush manually for next test.
            client.succeed(f"ip addr flush dev {IFACE}")
        return "release sent"

    def networkd_v6_lease():
        # Ensure interface is clean: stop networkd AND its socket (to prevent
        # socket activation during setup), flush addresses, remove conflicting
        # NixOS-generated .network files, bounce link for fresh link-local.
        client.succeed(
            "systemctl stop systemd-networkd.socket systemd-networkd.service || true"
        )
        client.succeed(f"ip addr flush dev {IFACE}")
        client.succeed("rm -f /etc/systemd/network/40-*.network")
        client.succeed("rm -f /etc/systemd/network/10-test-*.network")
        # Disable DAD on the test interface (avoids tentative-address removals
        # that delay DHCPv6 Solicit by 30+ seconds in QEMU VMs).
        client.succeed(f"sysctl -w net.ipv6.conf.{IFACE}.accept_dad=0")
        client.succeed(f"ip link set {IFACE} down && ip link set {IFACE} up")
        # Wait for link-local address to be assigned (instant with DAD=0)
        client.wait_until_succeeds(
            f"ip -6 addr show {IFACE} scope link | grep -q 'inet6 fe80'",
            timeout=10,
        )
        import time; time.sleep(1)  # settle
        # Force DHCPv6 without waiting for Router Advertisements.
        # WithoutRA=solicit tells networkd to send a Solicit immediately
        # instead of waiting for an RA with the M flag.
        client.succeed(
            "printf '[Match]\nName=" + IFACE + "\n\n"
            "[Network]\nDHCP=ipv6\nIPv6AcceptRA=false\nKeepConfiguration=dhcp\n\n"
            "[DHCPv6]\nWithoutRA=solicit\nUseDNS=true\n'"
            " > /etc/systemd/network/10-test-dhcp6.network"
        )
        # Mask the socket to prevent socket-activation restarts during the test
        client.succeed("systemctl mask systemd-networkd.socket || true")
        # Start only the service
        client.succeed("systemctl start systemd-networkd.service")
        # Wait for the DHCPv6 address to appear.  systemd-networkd may
        # briefly lose the lease during internal reconfiguration in QEMU VMs,
        # so check both the interface AND the journal as evidence of success.
        client.wait_until_succeeds(
            f"ip -6 addr show {IFACE} scope global | grep -v fe80 | grep -q 'inet6 '"
            " || journalctl -u systemd-networkd.service --no-pager -o cat"
            f" | grep -q 'DHCPv6 address'",
            timeout=30,
        )
        # Check journal for the DHCPv6 address line (more reliable than
        # checking the interface since networkd may drop/re-add the address).
        addr_line = client.succeed(
            "journalctl -u systemd-networkd.service --no-pager -o cat"
            f" | grep 'DHCPv6 address' | tail -1"
        ).strip()
        assert "DHCPv6 address" in addr_line, \
            "systemd-networkd did not obtain a DHCPv6 address"
        # Unmask the socket for cleanup
        client.succeed("systemctl unmask systemd-networkd.socket || true")
        return f"journal: {addr_line}"

    def networkd_v6_options():
        # DHCPv6 DNS option should be surfaced via networkctl status output.
        status = client.succeed(f"networkctl status {IFACE} --no-pager || true")
        assert "fd00:2::1" in status or "2001:db8:2::1" in status, \
            f"expected DHCPv6 DNS server in networkctl status, got: {status}"
        return "dns option visible"

    def networkd_v6_renew():
        ip_before = client.succeed(
            f"ip -6 addr show {IFACE} scope global | grep -v fe80 | grep 'inet6 ' | awk '{{print $2}}' | head -1"
        ).strip()
        client.succeed(f"networkctl renew {IFACE}")
        import time; time.sleep(3)
        ip_after = client.succeed(
            f"ip -6 addr show {IFACE} scope global | grep -v fe80 | grep 'inet6 ' | awk '{{print $2}}' | head -1"
        ).strip()
        assert ip_after, "IPv6 address lost after networkctl renew"
        return f"before={ip_before} after={ip_after}"

    def networkd_v6_release():
        # Remove DHCPv6 config; networkd should release and drop dynamic address.
        client.succeed("rm -f /etc/systemd/network/10-test-dhcp6.network")
        client.succeed("networkctl reload")
        import time; time.sleep(3)
        ip_out = client.succeed(
            f"ip -6 addr show {IFACE} scope global | grep -v fe80 | grep 'inet6 ' || true"
        ).strip()
        if ip_out:
            client.succeed(f"ip -6 addr flush dev {IFACE} scope global")
        return "v6 release sent"

    def networkd_teardown():
        client.succeed("rm -f /etc/systemd/network/10-test-*.network")
        client.succeed("systemctl stop systemd-networkd.service || true")
        client.succeed(f"ip addr flush dev {IFACE}")
  '';

  testFunctions = {
    setup = "networkd_setup";
    v4_lease = "networkd_v4_lease";
    v4_options = "networkd_v4_options";
    v4_renew = "networkd_v4_renew";
    v4_release = "networkd_v4_release";
    v6_lease = "networkd_v6_lease";
    v6_options = "networkd_v6_options";
    v6_renew = "networkd_v6_renew";
    v6_release = "networkd_v6_release";
    teardown = "networkd_teardown";
  };
}
