# ---------------------------------------------------------------------------
# Shared Python helpers for dora NixOS VM integration tests.
#
# This file is interpolated verbatim into the testScript of each NixOS test.
# It provides:
#   - MatrixResults: structured result collection + pretty-printing
#   - reset_client_interface: tear down / reconfigure the test interface
#   - wait_dora_ready: block until dora is listening on DHCP ports
#   - timed: decorator / context manager for recording test duration
# ---------------------------------------------------------------------------

import json, os, time, traceback

# ── Result collection ──────────────────────────────────────────────────────

class MatrixResults:
    """Collects per-client, per-protocol, per-test results into a JSON-
    serialisable structure and can render terminal / Markdown tables."""

    def __init__(self, backend, dora_version="dev"):
        self.backend = backend
        self.data = {
            "meta": {
                "backend": backend,
                "dora_version": dora_version,
                "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "test_duration_ms": 0,
            },
            "clients": {},
            "summary": {"total": 0, "passed": 0, "failed": 0, "skipped": 0},
        }
        self._start = time.monotonic()

    def record(self, client, protocol, test, passed, duration_ms=0, details=""):
        c = self.data["clients"].setdefault(client, {})
        p = c.setdefault(protocol, {})
        status = "pass" if passed else "fail"
        p[test] = {"status": status, "duration_ms": int(duration_ms), "details": details}
        self.data["summary"]["total"] += 1
        if passed:
            self.data["summary"]["passed"] += 1
        else:
            self.data["summary"]["failed"] += 1

    def record_skip(self, client, protocol, test, reason=""):
        c = self.data["clients"].setdefault(client, {})
        p = c.setdefault(protocol, {})
        p[test] = {"status": "skip", "duration_ms": 0, "details": reason}
        self.data["summary"]["total"] += 1
        self.data["summary"]["skipped"] += 1

    def _finalise(self):
        self.data["meta"]["test_duration_ms"] = int(
            (time.monotonic() - self._start) * 1000
        )

    def to_json(self):
        self._finalise()
        return json.dumps(self.data, indent=2)

    def write_json(self, path):
        self._finalise()
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "w") as fh:
            json.dump(self.data, fh, indent=2)
        print(f"[matrix] results written to {path}")

    # ── Pretty printing ────────────────────────────────────────────────

    # Unicode box-drawing glyphs
    _PASS  = "\033[32m pass \033[0m"
    _FAIL  = "\033[1;31m FAIL \033[0m"
    _SKIP  = "\033[33m skip \033[0m"
    _NA    = "\033[90m  --  \033[0m"

    # Plain variants for Markdown / log files
    _P_PASS  = "pass"
    _P_FAIL  = "FAIL"
    _P_SKIP  = "skip"
    _P_NA    = " -- "

    # Columns in the matrix
    TEST_COLS = ["lease", "options", "renew", "release", "load"]

    def _status_cell(self, st, plain=False):
        if st is None:
            return self._P_NA if plain else self._NA
        s = st.get("status", "skip")
        if plain:
            return {"pass": self._P_PASS, "fail": self._P_FAIL, "skip": self._P_SKIP}.get(s, s)
        return {"pass": self._PASS, "fail": self._FAIL, "skip": self._SKIP}.get(s, s)

    def _iter_rows(self):
        """Yield (client_name, protocol, row_dict) tuples."""
        for client_name in sorted(self.data["clients"]):
            protocols = self.data["clients"][client_name]
            for proto in sorted(protocols):
                tests = protocols[proto]
                yield client_name, proto, tests

    def print_matrix(self):
        """Print an ANSI-coloured matrix table to stdout."""
        self._finalise()
        hdr = (
            f"\n{'=' * 78}\n"
            f"  DHCP Client Compatibility Matrix  [{self.backend}]\n"
            f"{'=' * 78}\n"
        )
        print(hdr)

        col_w = 7
        name_w = 22
        proto_w = 4
        header = f"{'Client':<{name_w}} {'Proto':<{proto_w}}"
        for c in self.TEST_COLS:
            header += f" {c:^{col_w}}"
        print(header)
        print("-" * len(header.replace("\033[", "").replace("[0m", "")))

        for client_name, proto, tests in self._iter_rows():
            row = f"{client_name:<{name_w}} {proto:<{proto_w}}"
            for col in self.TEST_COLS:
                cell = self._status_cell(tests.get(col))
                row += f" {cell}"
            print(row)

        s = self.data["summary"]
        dur = self.data["meta"]["test_duration_ms"] / 1000.0
        print(f"\n{'=' * 78}")
        print(
            f"  Total: {s['total']}  "
            f"\033[32mPassed: {s['passed']}\033[0m  "
            f"\033[1;31mFailed: {s['failed']}\033[0m  "
            f"\033[33mSkipped: {s['skipped']}\033[0m  "
            f"Duration: {dur:.1f}s"
        )
        print(f"{'=' * 78}\n")

    def markdown_table(self):
        """Return a GitHub-flavoured Markdown table string."""
        self._finalise()
        lines = [f"## DHCP Client Compatibility Matrix [{self.backend}]\n"]
        hdr = "| Client | Proto |"
        sep = "|--------|-------|"
        for c in self.TEST_COLS:
            hdr += f" {c} |"
            sep += "------|"
        lines.append(hdr)
        lines.append(sep)

        for client_name, proto, tests in self._iter_rows():
            row = f"| {client_name} | {proto} |"
            for col in self.TEST_COLS:
                cell = self._status_cell(tests.get(col), plain=True)
                row += f" {cell} |"
            lines.append(row)

        s = self.data["summary"]
        lines.append("")
        lines.append(
            f"**Total: {s['total']}** | "
            f"Passed: {s['passed']} | "
            f"Failed: {s['failed']} | "
            f"Skipped: {s['skipped']}"
        )
        return "\n".join(lines)

    def write_markdown(self, path):
        md = self.markdown_table()
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "w") as fh:
            fh.write(md)
        print(f"[matrix] markdown written to {path}")


# ── Interface management ───────────────────────────────────────────────────

def reset_client_interface(vm, iface, mac):
    """Flush addresses, set a fresh MAC, bring the interface back up.

    Also stops systemd-networkd (and its socket) so it doesn't re-apply
    addresses from the NixOS-generated 40-eth1.network file while other
    DHCP clients run.  The systemd-networkd client test re-enables both.
    """
    # Stop networkd AND its socket to prevent socket activation during reset
    vm.succeed("systemctl stop systemd-networkd.socket systemd-networkd.service || true")
    vm.succeed(f"ip link set {iface} down")
    vm.succeed(f"ip addr flush dev {iface}")
    vm.succeed(f"ip link set {iface} address {mac}")
    vm.succeed(f"ip link set {iface} up")
    # Kill any stale DHCP client processes
    vm.succeed("pkill -9 dhcpcd  || true")
    vm.succeed("pkill -9 udhcpc  || true")
    vm.succeed("pkill -9 dhcpm   || true")
    # Small settle time for link-local DAD etc.
    import time as _t
    _t.sleep(1)


def add_static_ip(vm, iface, ip, prefix=24):
    """Re-add a static IP for tool-based clients that need a bind address."""
    vm.succeed(f"ip addr add {ip}/{prefix} dev {iface} || true")


def add_static_ip6(vm, iface, ip, prefix=64):
    """Re-add a static IPv6 for tool-based clients that need a bind address."""
    vm.succeed(f"ip addr add {ip}/{prefix} dev {iface} || true")


# ── Server readiness ───────────────────────────────────────────────────────

def wait_standalone_ready(server):
    """Wait for a standalone dora instance to be fully ready."""
    server.wait_for_unit("dora.service")
    server.wait_until_succeeds("ss -lun | grep -q ':67'", timeout=30)
    server.wait_until_succeeds("ss -lun | grep -q ':547'", timeout=30)


def wait_nats_cluster_ready(dhcp1, dhcp2, client):
    """Wait for both NATS servers and both dora instances in a cluster."""
    dhcp1.wait_for_unit("nats.service")
    dhcp2.wait_for_unit("nats.service")
    dhcp1.wait_for_open_port(4222)
    dhcp2.wait_for_open_port(4222)

    dhcp1.wait_for_unit("dora.service")
    dhcp2.wait_for_unit("dora.service")
    dhcp1.wait_until_succeeds("ss -lun | grep -q ':67'", timeout=30)
    dhcp2.wait_until_succeeds("ss -lun | grep -q ':67'", timeout=30)
    dhcp1.wait_until_succeeds("ss -lun | grep -q ':547'", timeout=30)
    dhcp2.wait_until_succeeds("ss -lun | grep -q ':547'", timeout=30)

    dhcp1.wait_until_succeeds(
        "nats --server nats://127.0.0.1:4222 account info >/dev/null 2>&1"
    )
    dhcp2.wait_until_succeeds(
        "nats --server nats://127.0.0.1:4222 account info >/dev/null 2>&1"
    )
    dhcp1.wait_until_succeeds(
        "nats --server nats://127.0.0.1:4222 kv info dora_host_options >/dev/null 2>&1"
        " || nats --server nats://127.0.0.1:4222 kv add dora_host_options >/dev/null 2>&1"
    )
    client.succeed("systemctl stop dhcpcd.service >/dev/null 2>&1 || true")


# ── Timed test wrapper ─────────────────────────────────────────────────────

def timed_test(fn, *args, **kwargs):
    """Run *fn*, return (passed: bool, duration_ms: int, details: str).

    If *fn* raises, the test is considered failed and the traceback is
    captured in *details*.
    """
    t0 = time.monotonic()
    try:
        details = fn(*args, **kwargs)
        if details is None:
            details = ""
        return True, int((time.monotonic() - t0) * 1000), str(details)
    except Exception as exc:
        tb = traceback.format_exc()
        return False, int((time.monotonic() - t0) * 1000), f"{exc}\n{tb}"
