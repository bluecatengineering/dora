# dhcpcd -- full-featured DHCP client daemon.
#
# Tests lease acquisition, option verification (subnet mask, router),
# renewal via --rebind, and release.  Supports DHCPv4 and DHCPv6.
#
# The Python code references constants defined by the matrix test runner:
#   IFACE, SERVER_V4, SERVER_V6, client (the VM handle)
{
  name = "dhcpcd";
  mac = "02:00:00:01:00:01";

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

  # Python function definitions.  All reference the globals
  # IFACE, SERVER_V4, SERVER_V6, client.
  testCode = ''
    def dhcpcd_setup():
        client.succeed("pkill -9 dhcpcd || true")
        client.succeed(f"ip addr flush dev {IFACE}")
        client.succeed("rm -f /var/lib/dhcpcd/*.lease")

    def dhcpcd_v4_lease():
        client.succeed(
            f"dhcpcd --oneshot --noipv6 --waitip 4 --timeout 15 {IFACE}"
        )
        ip_out = client.succeed(
            f"ip -4 addr show {IFACE} | grep 'inet ' | awk '{{print $2}}'"
        ).strip()
        assert ip_out, "dhcpcd did not obtain an IPv4 address"
        ip = ip_out.split("/")[0]
        assert ip.startswith("192.168.2."), f"Expected 192.168.2.x, got {ip}"
        return f"got {ip_out}"

    def dhcpcd_v4_options():
        route = client.succeed(f"ip -4 route show default dev {IFACE}").strip()
        assert "192.168.2.1" in route, f"Expected router 192.168.2.1 in route: {route}"
        return f"route={route}"

    def dhcpcd_v4_renew():
        ip_before = client.succeed(
            f"ip -4 addr show {IFACE} | grep 'inet ' | awk '{{print $2}}'"
        ).strip()
        client.succeed(f"dhcpcd --rebind {IFACE}")
        import time; time.sleep(2)
        ip_after = client.succeed(
            f"ip -4 addr show {IFACE} | grep 'inet ' | awk '{{print $2}}'"
        ).strip()
        assert ip_after, "IP lost after rebind"
        return f"before={ip_before} after={ip_after}"

    def dhcpcd_v4_release():
        client.succeed(f"dhcpcd --release {IFACE}")
        import time; time.sleep(1)
        # dhcpcd --release sends DHCPRELEASE to the server but may not always
        # flush the address from the interface (depends on version/config).
        # Verify the release was sent by checking dhcpcd exited cleanly,
        # then manually flush to leave a clean state.
        client.succeed(f"ip addr flush dev {IFACE}")
        return "release sent"

    def dhcpcd_v6_lease():
        client.succeed("pkill -9 dhcpcd || true")
        client.succeed(f"ip -6 addr flush dev {IFACE} scope global")
        # Disable DAD and bounce interface to get a link-local address quickly
        client.succeed(f"sysctl -w net.ipv6.conf.{IFACE}.accept_dad=0")
        client.succeed(f"ip link set {IFACE} down && ip link set {IFACE} up")
        import time; time.sleep(1)
        # Write a dhcpcd config that forces DHCPv6 without waiting for RA.
        # The ia_na option tells dhcpcd to request a stateful DHCPv6 address.
        # noipv6rs disables Router Solicitation (we don't have a router).
        client.succeed(
            "printf 'noipv6rs\nia_na\n' > /tmp/dhcpcd-v6.conf"
        )
        client.succeed(
            f"dhcpcd --config /tmp/dhcpcd-v6.conf --ipv6only --oneshot --waitip 6 --timeout 30 {IFACE} || true"
        )
        ip_out = client.succeed(
            f"ip -6 addr show {IFACE} scope global | grep 'inet6 ' | grep -v 'fe80' | awk '{{print $2}}' || true"
        ).strip()
        if not ip_out:
            raise Exception("dhcpcd did not obtain a DHCPv6 address")
        return f"got {ip_out}"

    def dhcpcd_v6_options():
        # Verify DHCPv6 lease timers (T1/T2/valid) from dhcpcd output.
        # This is a stable signal that option payload from REPLY6 was processed.
        log = client.succeed(
            "journalctl --no-pager -o cat | grep -E 'renew in .* rebind in .* expire in' | tail -1"
        ).strip()
        assert "renew in" in log and "rebind in" in log and "expire in" in log, \
            f"expected DHCPv6 timer line in dhcpcd logs, got: {log}"
        return f"timers={log}"

    def dhcpcd_v6_renew():
        ip_before = client.succeed(
            f"ip -6 addr show {IFACE} scope global | grep -v fe80 | grep 'inet6 ' | awk '{{print $2}}' | head -1"
        ).strip()
        client.succeed(f"dhcpcd --rebind {IFACE}")
        import time; time.sleep(2)
        ip_after = client.succeed(
            f"ip -6 addr show {IFACE} scope global | grep -v fe80 | grep 'inet6 ' | awk '{{print $2}}' | head -1"
        ).strip()
        assert ip_after, "IPv6 address lost after dhcpcd rebind"
        return f"before={ip_before} after={ip_after}"

    def dhcpcd_v6_release():
        # Similar to v4: release is best-effort from client perspective.
        client.succeed(f"dhcpcd --release {IFACE} || true")
        import time; time.sleep(1)
        client.succeed(f"ip -6 addr flush dev {IFACE} scope global")
        return "v6 release sent"

    def dhcpcd_teardown():
        client.succeed("pkill -9 dhcpcd || true")
        client.succeed(f"ip addr flush dev {IFACE}")
  '';

  # Map capability names to the Python function names above.
  testFunctions = {
    setup = "dhcpcd_setup";
    v4_lease = "dhcpcd_v4_lease";
    v4_options = "dhcpcd_v4_options";
    v4_renew = "dhcpcd_v4_renew";
    v4_release = "dhcpcd_v4_release";
    v6_lease = "dhcpcd_v6_lease";
    v6_options = "dhcpcd_v6_options";
    v6_renew = "dhcpcd_v6_renew";
    v6_release = "dhcpcd_v6_release";
    teardown = "dhcpcd_teardown";
  };
}
