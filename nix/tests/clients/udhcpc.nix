# udhcpc -- BusyBox lightweight DHCP client (buildroot / embedded).
#
# This is the default DHCP client in buildroot and most embedded Linux
# systems.  It runs in one-shot mode and calls a user-supplied script
# to apply the lease.  We write a script that saves all DHCP options
# to a JSON file so we can verify them.
{
  name = "udhcpc";
  mac = "02:00:00:01:00:02";

  capabilities = {
    v4_lease = true;
    v4_options = true;
    v4_renew = false;
    v4_release = false;
    v6_lease = false;
    v6_options = false;
    v6_renew = false;
    v6_release = false;
    load = false;
  };

  testCode = ''
    def udhcpc_setup():
        client.succeed("pkill -9 udhcpc || true")
        client.succeed(f"ip addr flush dev {IFACE}")
        # Write a udhcpc default-script that applies lease and records options.
        client.succeed(
            "cat > /tmp/udhcpc-script.sh << 'SCRIPT'\n"
            "#!/bin/sh\n"
            "RESULT=/tmp/udhcpc-result.json\n"
            "case \"$1\" in\n"
            "  bound|renew)\n"
            "    ip addr flush dev \"$interface\"\n"
            "    ip addr add \"$ip/$mask\" dev \"$interface\"\n"
            "    if [ -n \"$router\" ]; then\n"
            "      while ip route del default dev \"$interface\" 2>/dev/null; do :; done\n"
            "      for r in $router; do ip route add default via \"$r\" dev \"$interface\"; done\n"
            "    fi\n"
            "    printf '{\"action\":\"%s\",\"ip\":\"%s\",\"mask\":\"%s\",\"router\":\"%s\",\"dns\":\"%s\",\"domain\":\"%s\",\"serverid\":\"%s\",\"lease\":\"%s\",\"boot_file\":\"%s\",\"siaddr\":\"%s\"}' "
            "\"$1\" \"$ip\" \"$mask\" \"$router\" \"$dns\" \"$domain\" \"$serverid\" \"$lease\" \"$boot_file\" \"$siaddr\" > \"$RESULT\"\n"
            "    ;;\n"
            "  deconfig)\n"
            "    ip addr flush dev \"$interface\"\n"
            "    ip link set \"$interface\" up\n"
            "    ;;\n"
            "esac\n"
            "SCRIPT\n"
            "chmod +x /tmp/udhcpc-script.sh"
        )
        client.succeed("rm -f /tmp/udhcpc-result.json")

    def udhcpc_v4_lease():
        client.succeed(
            f"busybox udhcpc -i {IFACE} -s /tmp/udhcpc-script.sh -n -q -t 5 -T 3"
        )
        result = client.succeed("cat /tmp/udhcpc-result.json")
        data = json.loads(result)
        ip = data.get("ip", "")
        assert ip.startswith("192.168.2."), f"Expected 192.168.2.x, got {ip}"
        return f"got {ip}/{data.get('mask', '?')}"

    def udhcpc_v4_options():
        result = client.succeed("cat /tmp/udhcpc-result.json")
        data = json.loads(result)
        errors = []
        mask = data.get("mask", "")
        # udhcpc may report mask as CIDR prefix (e.g. "24") or dotted notation
        if mask not in ("255.255.255.0", "24"):
            errors.append(f"mask={mask}, expected 255.255.255.0 or 24")
        if "192.168.2.1" not in data.get("router", ""):
            errors.append(f"router={data.get('router')}, expected 192.168.2.1")
        if errors:
            raise AssertionError("; ".join(errors))
        return f"mask={data['mask']} router={data['router']} dns={data.get('dns', str())}"

    def udhcpc_teardown():
        client.succeed("pkill -9 udhcpc || true")
        client.succeed(f"ip addr flush dev {IFACE}")
  '';

  testFunctions = {
    setup = "udhcpc_setup";
    v4_lease = "udhcpc_v4_lease";
    v4_options = "udhcpc_v4_options";
    teardown = "udhcpc_teardown";
  };
}
