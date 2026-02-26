{
  description = "Dora - A rust DHCP server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
      flake-parts,
    }@inputs:
    flake-parts.lib.mkFlake
      {
        inherit inputs;
      }
      {
        flake = {
          nixosModules = rec {
            default = dora;
            dora = ./modules/default.nix;
          };
        };
        systems = flake-utils.lib.allSystems;
        perSystem =
          {
            config,
            self,
            inputs,
            pkgs,
            system,
            ...
          }:
          let
            overlays = [ (import rust-overlay) ];
            pkgs = import nixpkgs {
              inherit system overlays;
            };
            doraPkg = pkgs.callPackage ./package.nix { };
            dhcpLoadtestPkg = pkgs.rustPlatform.buildRustPackage {
              pname = "dhcp-loadtest";
              version = "0.1.0";
              src = ./.;
              cargoLock = {
                lockFile = ./Cargo.lock;
              };
              cargoBuildFlags = [
                "-p"
                "dhcp-loadtest"
              ];
              cargoCheckFlags = [
                "-p"
                "dhcp-loadtest"
              ];
              doCheck = false;
            };
          in
          {
            devShells.default = pkgs.callPackage ./shell.nix { };
            packages = {
              default = doraPkg;
              dhcp-loadtest = dhcpLoadtestPkg;
            };
            checks = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
              dhcp-nats-jetstream-load = import ./nix/tests/dhcp-nats-jetstream-load.nix {
                inherit pkgs;
                dora = doraPkg;
                dhcpLoadtest = dhcpLoadtestPkg;
              };
            };
          };
      };
}
