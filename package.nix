{
  pkgs ? import <nixpkgs> {},
  lib,
  ...
}:
pkgs.rustPlatform.buildRustPackage {
  pname = "dora";
  version = (builtins.fromTOML (lib.readFile ./bin/Cargo.toml)).package.version;

  src = ./.;
  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  # disable tests
  checkType = "debug";
  doCheck = false;

  nativeBuildInputs = with pkgs; [
    installShellFiles
    pkg-config
  ];
  buildInputs = with pkgs; [
    pkg-config
    openssl

    (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
  ];

  # We need to create a testing database for the binary to compile
  # because of post-build tests.
  DATABASE_URL = "sqlite://./em.db?mode=rwc";
  preBuild = with pkgs; ''
    ${sqlx-cli}/bin/sqlx database create
    ${sqlx-cli}/bin/sqlx migrate run
  '';

  # postInstall = with lib; ''
  #   installShellCompletion --cmd ${pname}\
  #     --bash ./autocompletion/${pname}.bash \
  #     --fish ./autocompletion/${pname}.fish \
  #     --zsh  ./autocompletion/_${pname}
  # '';
}
