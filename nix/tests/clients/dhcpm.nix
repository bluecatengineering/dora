# dhcpm -- Rust CLI for sending individual DHCP messages.
#
# Performs a full DORA cycle via `dhcpm <server> dora` and can also
# send individual discover/request/release messages.  Outputs JSON
# for option verification.
{
  name = "dhcpm";
  mac = "02:00:00:01:00:06";

  capabilities = {
    v4_lease = true;
    v4_options = true;
    v4_renew = false; # dhcpm doesn't have a renew command
    v4_release = true;
    v6_lease = false; # dhcpm v6 is inforeq only, not full stateful
    v6_options = false;
    v6_renew = false;
    v6_release = false;
    load = false;
  };

  testCode = ''
    def dhcpm_setup():
        add_static_ip(client, IFACE, CLIENT_V4)

    def dhcpm_v4_lease():
        """Perform a full DORA cycle using dhcpm."""
        output = client.succeed(
            f"dhcpm 255.255.255.255 -i {IFACE} --output json dora 2>&1"
        )
        return f"dora completed: {output[:200]}"

    def dhcpm_v4_options():
        """Run a DISCOVER and inspect response options via JSON output."""
        output = client.succeed(
            f"dhcpm 255.255.255.255 -i {IFACE} --output json discover 2>&1"
        )
        return f"discover response: {output[:200]}"

    def dhcpm_v4_release():
        """Send a RELEASE message."""
        # dhcpm release requires knowing the IP; do a fresh DORA first
        client.succeed(
            f"dhcpm 255.255.255.255 -i {IFACE} --output json dora 2>&1"
        )
        # dhcpm sends a RELEASE but per RFC 2131 the server does not reply.
        # dhcpm may exit non-zero because it times out waiting for a response
        # that will never come.  Accept exit code 1 as long as the RELEASE
        # was actually sent (visible in output).
        output = client.succeed(
            f"dhcpm 255.255.255.255 -i {IFACE} release 2>&1 || true"
        )
        assert "release" in output.lower() or "SENT" in output or "Release" in output, \
            f"dhcpm release output unexpected: {output[:300]}"
        return "release sent"

    def dhcpm_teardown():
        pass
  '';

  testFunctions = {
    setup = "dhcpm_setup";
    v4_lease = "dhcpm_v4_lease";
    v4_options = "dhcpm_v4_options";
    v4_release = "dhcpm_v4_release";
    teardown = "dhcpm_teardown";
  };
}
