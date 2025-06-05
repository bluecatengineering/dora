{
  description = "Dora - A rust DHCP server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    flake-utils,
    flake-parts,
  } @ inputs:
    flake-parts.lib.mkFlake {
      inherit inputs;
    } {
      flake = {
        nixosModules = rec {
          default = dora;
          dora = ./modules/default.nix;
        };
      };
      systems =
        flake-utils.lib.allSystems;
      perSystem = {
        config,
        self,
        inputs,
        pkgs,
        system,
        ...
      }: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
      in {
        devShells.default = pkgs.callPackage ./shell.nix {};
        packages.default = pkgs.callPackage ./package.nix {};
      };
    };
}
