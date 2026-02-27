# Universal DHCP client compatibility matrix test.
#
# This NixOS VM test boots a dora DHCP server (in either standalone or
# NATS-clustered mode) and exercises every supported DHCP client against
# it, collecting structured pass/fail/skip results into a JSON matrix
# that is stored as a build artifact.
#
# Usage from flake.nix:
#   import ./nix/tests/dhcp-client-matrix.nix {
#     inherit pkgs dora dhcpLoadtest;
#     mode = "standalone";  # or "nats"
#   }
#
# The derivation output contains:
#   $out/results.json  -- machine-readable result matrix
#   $out/matrix.md     -- GitHub-flavoured Markdown table
{
  pkgs,
  dora,
  dhcpLoadtest,
  mode ? "standalone",
}:

let
  lib = pkgs.lib;

  testLib = import ./lib {
    inherit pkgs dora dhcpLoadtest;
  };

  clientDefs = (import ./clients).all;

  # The network constants, shared between Nix node config and Python tests.
  net = {
    serverV4 = "192.168.2.2";
    serverV6 = "fd00:2::2";
    clientV4 = "192.168.2.10";
    clientV6 = "fd00:2::10";
    clientMac = "02:00:00:00:10:01";
    iface = "eth1";
  };

  # For NATS mode we need a second server and a control VLAN.
  natsNet = {
    server1ControlIp = "192.168.1.4";
    server2ControlIp = "192.168.1.5";
    server1DhcpIp = "192.168.2.2";
    server2DhcpIp = "192.168.2.3";
    server1V6 = "fd00:2::2";
    server2V6 = "fd00:2::3";
  };

  # ── Build the Python test code for all clients ───────────────────────

  # Concatenate all client testCode blocks into one big Python string.
  allClientCode = lib.concatMapStringsSep "\n\n" (c: c.testCode) clientDefs;

  # Generate the Python test-dispatch table.
  # For each client, generate a dict mapping test names to function refs.
  mkClientEntry =
    c:
    let
      capsToTest = builtins.attrNames c.capabilities;
      enabledTests = builtins.filter (t: c.capabilities.${t} or false) capsToTest;
      fnEntries = map (
        t: if builtins.hasAttr t c.testFunctions then "\"${t}\": ${c.testFunctions.${t}}" else ""
      ) enabledTests;
      fnEntriesClean = builtins.filter (e: e != "") fnEntries;
      skipEntries = map (t: if !(c.capabilities.${t} or false) then "\"${t}\"" else "") capsToTest;
      skipEntriesClean = builtins.filter (e: e != "") skipEntries;
    in
    ''
      {
          "name": "${c.name}",
          "mac": "${c.mac}",
          "setup": ${
            if builtins.hasAttr "setup" c.testFunctions then c.testFunctions.setup else "lambda: None"
          },
          "teardown": ${
            if builtins.hasAttr "teardown" c.testFunctions then c.testFunctions.teardown else "lambda: None"
          },
          "tests": {${lib.concatStringsSep ", " fnEntriesClean}},
          "skip": [${lib.concatStringsSep ", " skipEntriesClean}],
      },
    '';

  clientTable = lib.concatMapStringsSep "\n" mkClientEntry clientDefs;

  # ── Determine which test categories exist ──────────────────────────

  # v4 tests and v6 tests are separated in reporting
  v4Tests = [
    "v4_lease"
    "v4_options"
    "v4_renew"
    "v4_release"
  ];
  v6Tests = [
    "v6_lease"
    "v6_options"
    "v6_renew"
    "v6_release"
  ];
  otherTests = [ "load" ];
  allTests = v4Tests ++ v6Tests ++ otherTests;

  # ── Build the NixOS test ───────────────────────────────────────────

  standaloneNodes = {
    server = testLib.mkStandaloneNode {
      instanceId = "1";
      dhcpIp = net.serverV4;
      dhcpV6 = net.serverV6;
      serverId = net.serverV4;
    };

    client = testLib.mkMatrixClientNode {
      clientIp = net.clientV4;
      clientV6 = net.clientV6;
      clientMac = net.clientMac;
    };
  };

  natsNodes = {
    dhcp1 = testLib.mkNatsNode {
      instanceId = "1";
      controlIp = natsNet.server1ControlIp;
      dhcpIp = natsNet.server1DhcpIp;
      dhcpV6 = natsNet.server1V6;
      serverId = natsNet.server1DhcpIp;
      peerNatsIp = natsNet.server2ControlIp;
    };

    dhcp2 = testLib.mkNatsNode {
      instanceId = "2";
      controlIp = natsNet.server2ControlIp;
      dhcpIp = natsNet.server2DhcpIp;
      dhcpV6 = natsNet.server2V6;
      serverId = natsNet.server2DhcpIp;
      peerNatsIp = natsNet.server1ControlIp;
    };

    client = testLib.mkNatsClientNode {
      clientIp = net.clientV4;
      clientV6 = net.clientV6;
      clientMac = net.clientMac;
    };
  };

  nodes = if mode == "standalone" then standaloneNodes else natsNodes;

  # Python test script
  testScript = ''
    # ── Shared helpers ──────────────────────────────────────────────
    ${testLib.testHelpers}

    # ── Constants ───────────────────────────────────────────────────
    IFACE      = "${net.iface}"
    SERVER_V4  = "${net.serverV4}"
    SERVER_V6  = "${net.serverV6}"
    CLIENT_V4  = "${net.clientV4}"
    CLIENT_V6  = "${net.clientV6}"
    MODE       = "${mode}"
    DHCPING_MAC = "02:00:00:01:00:07"

    # ── All client test functions ───────────────────────────────────
    ${allClientCode}

    # ── Client dispatch table ───────────────────────────────────────
    CLIENT_DEFS = [
    ${clientTable}
    ]

    # ── Test categories ─────────────────────────────────────────────
    V4_TESTS = ${builtins.toJSON v4Tests}
    V6_TESTS = ${builtins.toJSON v6Tests}
    OTHER_TESTS = ${builtins.toJSON otherTests}
    ALL_TESTS = V4_TESTS + V6_TESTS + OTHER_TESTS

    # ── Result collector ────────────────────────────────────────────
    results = MatrixResults(backend=MODE)

    # ── Boot and wait for infrastructure ────────────────────────────
    start_all()

    ${
      if mode == "standalone" then
        ''
          with subtest("Standalone dora server is ready"):
              wait_standalone_ready(server)
        ''
      else
        ''
          with subtest("NATS cluster + dora are ready"):
              wait_nats_cluster_ready(dhcp1, dhcp2, client)
              dhcp1.wait_until_succeeds(
                  "journalctl -u dora.service --no-pager -o cat | grep -q 'NATS connection established for nats mode'"
              )
              dhcp2.wait_until_succeeds(
                  "journalctl -u dora.service --no-pager -o cat | grep -q 'NATS connection established for nats mode'"
              )
        ''
    }

    # ── Run each client's tests ─────────────────────────────────────
    for cdef in CLIENT_DEFS:
        cname = cdef["name"]

        with subtest(f"Client: {cname}"):
            # Reset interface with client-specific MAC
            reset_client_interface(client, IFACE, cdef["mac"])

            # Run setup
            try:
                cdef["setup"]()
            except Exception as exc:
                print(f"[matrix] {cname} setup failed: {exc}")
                # Record all tests as failed
                for t in ALL_TESTS:
                    if t.startswith("v4_"): _proto, _col = "v4", t[3:]
                    elif t.startswith("v6_"): _proto, _col = "v6", t[3:]
                    else: _proto, _col = "other", t
                    if t in cdef["tests"]:
                        results.record(cname, _proto, _col, False, 0, f"setup failed: {exc}")
                    elif t in cdef["skip"]:
                        results.record_skip(cname, _proto, _col, "not supported")
                continue

            # Run each test
            for test_name in ALL_TESTS:
                if test_name.startswith("v4_"):
                    proto = "v4"
                    col = test_name[3:]    # v4_lease -> lease
                elif test_name.startswith("v6_"):
                    proto = "v6"
                    col = test_name[3:]    # v6_lease -> lease
                else:
                    proto = "other"
                    col = test_name        # load -> load

                if test_name in cdef.get("skip", []):
                    results.record_skip(cname, proto, col, "not supported")
                    continue

                # DHCPv6 Solicit is only handled in NATS mode.  In standalone
                # mode, skip all v6 tests that require a full stateful exchange.
                if MODE == "standalone" and proto == "v6" and col in ("lease", "renew", "release"):
                    results.record_skip(cname, proto, col, "v6 stateful not supported in standalone mode")
                    continue

                if test_name not in cdef.get("tests", {}):
                    results.record_skip(cname, proto, col, "not implemented")
                    continue

                fn = cdef["tests"][test_name]
                passed, duration_ms, details = timed_test(fn)
                results.record(cname, proto, col, passed, duration_ms, details)
                status_str = "PASS" if passed else "FAIL"
                print(f"[matrix] {cname}/{proto}/{col}: {status_str} ({duration_ms}ms)")
                if not passed:
                    print(f"[matrix]   details: {details[:500]}")

            # Run teardown
            try:
                cdef["teardown"]()
            except Exception:
                pass  # teardown failures are non-fatal

    # ── Write results ───────────────────────────────────────────────
    out_dir = os.environ.get("out", "/tmp")
    results.write_json(os.path.join(out_dir, "results.json"))
    results.write_markdown(os.path.join(out_dir, "matrix.md"))
    results.print_matrix()

    # ── Final assertions ────────────────────────────────────────────
    summary = results.data["summary"]
    print(f"\n[matrix] Final: {summary['passed']}/{summary['total']} passed, "
          f"{summary['failed']} failed, {summary['skipped']} skipped")

    # The test passes if there are no failures (skips are ok).
    assert summary["failed"] == 0, \
        f"Client matrix has {summary['failed']} failure(s) -- see results.json"
  '';
in
pkgs.testers.nixosTest {
  name = "dhcp-client-matrix-${mode}";
  inherit nodes;
  # The matrix test uses dynamic dispatch (dicts of callables) which
  # the NixOS test driver's mypy pass cannot type-check.  The shared
  # helpers file also re-imports stdlib modules that the driver exposes,
  # triggering the linter's redefinition warning.
  skipTypeCheck = true;
  skipLint = true;
  testScript = testScript;
}
