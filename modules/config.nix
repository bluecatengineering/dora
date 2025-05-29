{
  lib,
  config,
  inputs,
  pkgs,
  ...
}:
with lib; let
  moduleName = "dora";
  cfg = config.services.${moduleName};
in
  mkIf cfg.enable {
    systemd.tmpfiles.rules = [
      # Ensure working directory
      "Z '/var/lib/dora' 774 root users - -"
      "d '/var/lib/dora' 774 root users - -"

      # Ensure configuration directory
      "Z '/etc/config/dora' 774 root users - -"
      "d '/etc/config/dora' 774 root users - -"
    ];

    # Run the dhcp server in a ystemd background service
    systemd.services.dora = {
      enable = true;
      description = "Dora - A rust DHCP server";
      documentation = [
        "https://github.com/bluecatengineering/dora"
        "dora --help"
      ];
      after = [
        "network.target"
      ];
      wantedBy = ["multi-user.target"];

      serviceConfig = with pkgs; let
        package = inputs.dora.packages.${system}.default;
      in {
        Type = "simple";
        User = "root";
        Group = "users";
        Environment = "PATH=/run/current-system/sw/bin";
        ExecStart = ''
          ${package}/bin/dora dora \
          -c /etc/config/dora/config.yaml \
          -d /var/lib/dora/leases.db \
          -vvv
        '';
        ExecStartPost = [
          "-${pkgs.coreutils}/bin/chown -R root:users /var/lib/dora"
          "-${pkgs.coreutils}/bin/chmod -R 774 /var/lib/dora"
        ];

        WorkingDirectory = "/var/lib/dora";

        StandardInput = "null";
        StandardOutput = "journal+console";
        StandardError = "journal+console";

        AmbientCapabilities = [
          # Allow service to open a tcp or unix socket to listen to.
          "CAP_NET_BIND_SERVICE"
          # "CAP_NET_ADMIN"
          # "CAP_SYS_ADMIN"
        ];
      };
    };

    environment.systemPackages = with pkgs; [
      # Add dora to environment because you never know.
      inputs.dora.packages.${system}.default
    ];
  }
