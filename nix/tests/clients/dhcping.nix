# dhcping -- minimal DHCP server probe / diagnostic tool.
#
# Sends a single DHCPREQUEST to check whether a DHCP server is
# running and responding.  This is only a reachability test.
{
  name = "dhcping";
  mac = "02:00:00:01:00:07";

  capabilities = {
    v4_lease = true;
    v4_options = false;
    v4_renew = false;
    v4_release = false;
    v6_lease = false;
    v6_options = false;
    v6_renew = false;
    v6_release = false;
    load = false;
  };

  testCode = ''
    def dhcping_setup():
        add_static_ip(client, IFACE, CLIENT_V4)

    def dhcping_v4_lease():
        """Probe the DHCP server and verify a response is received."""
        output = client.succeed(
            f"dhcping -s {SERVER_V4} -c {CLIENT_V4} -h {DHCPING_MAC} -t 5 2>&1"
        )
        # dhcping prints "Got answer from: <ip>" on success
        assert "got answer" in output.lower() or SERVER_V4 in output, \
            f"dhcping did not get a response: {output}"
        return f"server responded: {output.strip()}"

    def dhcping_teardown():
        pass
  '';

  testFunctions = {
    setup = "dhcping_setup";
    v4_lease = "dhcping_v4_lease";
    teardown = "dhcping_teardown";
  };
}
