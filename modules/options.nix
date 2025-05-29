{lib, ...}:
with lib; let
  moduleName = "dora";
in {
  ## Options
  options.services.${moduleName} = {
    enable = mkEnableOption "Enable ${moduleName}.";
  };
}
