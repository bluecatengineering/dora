{pkgs ? import <nixpkgs> {}, ...}:
pkgs.mkShell {
  buildInputs = with pkgs.buildPackages; [
    pkg-config
    openssl

    (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
    rust-analyzer
  ];

  # Production
  # DATABASE_URL = "sqlite:////var/lib/dora/leases.db?mode=rwc";
  # DBEE_CONNECTIONS = "[
  #   {
  #     \"name\": \"dora_db\",
  #     \"type\": \"sqlite\",
  #     \"url\": \"/var/lib/dora/leases.db?mode=rwc\"
  #   }
  # ]";

  # Development
  DATABASE_URL = "sqlite://./em.db?mode=rwc";
  DBEE_CONNECTIONS = "[
    {
      \"name\": \"dora_db\",
      \"type\": \"sqlite\",
      \"url\": \"./em.db?mode=rwc\"
    }
  ]";

  # Fix jemalloc-sys build error on nixos
  # _FORTIFY_SOURCE = 0;
  hardeningDisable = ["all"];
}
