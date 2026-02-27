# Index of all DHCP client test definitions for the compatibility matrix.
#
# Each entry describes a DHCP client: its name, MAC address, capabilities,
# the Python test functions it provides, and a mapping of capability names
# to function names.
#
# Clients are listed in test execution order (real OS clients first, then
# testing tools, then diagnostic utilities).
{
  all = [
    # -- Real OS DHCP clients --
    (import ./dhcpcd.nix)
    (import ./udhcpc.nix)
    (import ./systemd-networkd.nix)

    # -- DHCP testing / load tools --
    (import ./dhcp-loadtest.nix)
    (import ./perfdhcp.nix)
    (import ./dhcpm.nix)

    # -- Diagnostic utilities --
    (import ./dhcping.nix)
  ];
}
