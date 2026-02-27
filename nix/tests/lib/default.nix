# Shared test library for dora DHCP server NixOS VM integration tests.
#
# Provides configuration generators, VM node builders, and Python test
# helpers that are reused across the NATS cluster test, the standalone
# test, and the universal client-compatibility matrix test.
{
  pkgs,
  dora,
  dhcpLoadtest,
}:

let
  doraConfigs = import ./dora-config.nix { inherit pkgs; };
  serverNodes = import ./server-node.nix { inherit pkgs dora doraConfigs; };
  clientNodes = import ./client-node.nix { inherit pkgs dhcpLoadtest; };

  # Python helpers as a string, ready to be interpolated into testScript.
  testHelpers = builtins.readFile ./test-script-helpers.py;
in
{
  inherit doraConfigs;

  # Server node builders
  inherit (serverNodes) mkStandaloneNode mkNatsNode;

  # Client node builders
  inherit (clientNodes) mkMatrixClientNode mkNatsClientNode;

  # Python test helper code (string to prepend to testScript)
  inherit testHelpers;
}
