# Nixos install

Install dora under nixos.

Make sure you have a configuration file,
otherwise the systemd-unit will fail.

```sh
touch /etc/config/dora/config.yaml
```

Add this flake url to your inputs in `flake.nix`.

```nix
inputs = {
  dora = {
    url = "github:pipelight/dora.nix";
    # inputs.nixpkgs.follows = "nixpkgs";
  };
};
```

Import the module.

```nix
imports = [
  inputs.dora.nixosModules.default
];
```

Then enable it somewhere in your configuration.

```nix
services.dora.enable = true;

```
