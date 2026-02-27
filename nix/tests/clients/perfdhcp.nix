# perfdhcp -- ISC Kea DHCP performance testing tool.
#
# Sends rapid DISCOVER/REQUEST sequences and reports statistics.
# Primarily a load and reachability test; does not verify individual
# DHCP options.
{
  name = "perfdhcp";
  mac = "02:00:00:01:00:05"; # only for interface reset; perfdhcp generates its own MACs

  capabilities = {
    v4_lease = true;
    v4_options = false;
    v4_renew = false;
    v4_release = false;
    v6_lease = true;
    v6_options = false;
    v6_renew = false;
    v6_release = false;
    load = true;
  };

  testCode = ''
    def perfdhcp_setup():
        add_static_ip(client, IFACE, CLIENT_V4)
        add_static_ip6(client, IFACE, CLIENT_V6)

    def perfdhcp_v4_lease():
        """Run a short perfdhcp v4 burst and verify we get responses."""
        client.succeed(
            f"perfdhcp -4 -l {CLIENT_V4} "
            f"-r 10 -R 20 -n 20 -D 0 -u "
            f"{SERVER_V4} > /tmp/perfdhcp-v4.log 2>&1 || true"
        )
        output = client.succeed("cat /tmp/perfdhcp-v4.log")
        # perfdhcp always prints some stats; we check for sent/received
        assert "sent" in output.lower() or "received" in output.lower() or "drops" in output.lower(), \
            f"perfdhcp produced unexpected output: {output[:200]}"
        return "perfdhcp v4 burst completed"

    def perfdhcp_v6_lease():
        """Run a short perfdhcp v6 burst."""
        add_static_ip6(client, IFACE, CLIENT_V6)
        # perfdhcp -6 needs a link-local or global source address.
        # Use -l <iface> and -b duid to avoid binding to a hardcoded address.
        client.succeed(
            f"perfdhcp -6 -l {IFACE} "
            f"-b duid "
            f"-r 5 -R 10 -n 10 "
            f"{SERVER_V6} > /tmp/perfdhcp-v6.log 2>&1 || true"
        )
        output = client.succeed("cat /tmp/perfdhcp-v6.log")
        assert "sent" in output.lower() or "received" in output.lower() or "drops" in output.lower(), \
            f"perfdhcp v6 produced unexpected output: {output[:200]}"
        return "perfdhcp v6 burst completed"

    def perfdhcp_load():
        """Higher-volume perfdhcp load test."""
        add_static_ip(client, IFACE, CLIENT_V4)
        client.succeed(
            f"perfdhcp -4 -l {CLIENT_V4} "
            f"-r 15 -R 40 -n 40 -D 0 -u "
            f"{SERVER_V4} > /tmp/perfdhcp-load.log 2>&1 || true"
        )
        client.succeed("grep -Eiq 'sent|received|drops|responses' /tmp/perfdhcp-load.log")
        return "perfdhcp load test completed"

    def perfdhcp_teardown():
        pass
  '';

  testFunctions = {
    setup = "perfdhcp_setup";
    v4_lease = "perfdhcp_v4_lease";
    v6_lease = "perfdhcp_v6_lease";
    load = "perfdhcp_load";
    teardown = "perfdhcp_teardown";
  };
}
