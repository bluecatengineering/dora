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
            checks = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux (
              let
                matrixArgs = {
                  inherit pkgs;
                  dora = doraPkg;
                  dhcpLoadtest = dhcpLoadtestPkg;
                };
                standaloneMatrix = import ./nix/tests/dhcp-client-matrix.nix (
                  matrixArgs // { mode = "standalone"; }
                );
                natsMatrix = import ./nix/tests/dhcp-client-matrix.nix (matrixArgs // { mode = "nats"; });
              in
              {
                # ── Existing NATS cluster integration test ──────────────
                dhcp-nats-jetstream-load = import ./nix/tests/dhcp-nats-jetstream-load.nix matrixArgs;

                # ── Client compatibility matrix tests ───────────────────
                dhcp-client-matrix-standalone = standaloneMatrix;
                dhcp-client-matrix-nats = natsMatrix;

                # ── Combined report (depends on both matrix tests) ──────
                # Build with: nix build .#checks.x86_64-linux.dhcp-matrix-report -L
                # Results in: result/{matrix.json,matrix.md,matrix.txt,
                #             standalone-results.json,nats-results.json}
                dhcp-matrix-report =
                  pkgs.runCommand "dhcp-matrix-report"
                    {
                      nativeBuildInputs = [ pkgs.python3 ];
                      standalone = standaloneMatrix;
                      nats = natsMatrix;
                    }
                    ''
                      mkdir -p $out

                      python3 ${./nix/format-matrix-results.py} \
                        --standalone "$standalone/results.json" \
                        --nats "$nats/results.json" \
                        --output-json "$out/matrix.json" \
                        --output-md "$out/matrix.md" \
                        --output-term "$out/matrix.txt" \
                        --no-color \
                      | tee "$out/summary.txt"

                      # Copy per-backend results for archival / diffing
                      cp "$standalone/results.json" "$out/standalone-results.json"
                      cp "$nats/results.json" "$out/nats-results.json"
                    '';
              }
            );
          };
      };
}
