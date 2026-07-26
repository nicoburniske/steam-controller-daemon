{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.scd;
  udevRules = pkgs.writeTextFile {
    name = "scd-udev-rules";
    destination = "/lib/udev/rules.d/72-scd.rules";
    text = ''
      SUBSYSTEM=="misc", KERNEL=="uinput", TAG+="uaccess", OWNER:="root", GROUP:="root", MODE:="0660"
      SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="28de", ATTR{idProduct}=="1304", TAG+="uaccess", OWNER:="root", GROUP:="root", MODE:="0660"
      SUBSYSTEM=="hidraw", KERNEL=="hidraw*", ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304", TAG+="uaccess", OWNER:="root", GROUP:="root", MODE:="0660"
      SUBSYSTEM=="tty", ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304", TAG-="uaccess", OWNER:="root", GROUP:="root", MODE:="0000"
      SUBSYSTEM=="input", ATTRS{id/vendor}=="28de", ATTRS{id/product}=="1304", TAG-="uaccess", OWNER:="root", GROUP:="root", MODE:="0000", ENV{LIBINPUT_IGNORE_DEVICE}="1"
    '';
  };
in {
  options.services.scd = {
    enable = lib.mkEnableOption "the Steam Controller userspace daemon";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix {};
      description = "The scd package to use.";
    };

    osk.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to run the on-screen keyboard in the graphical session.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [cfg.package];
    hardware.uinput.enable = true;
    services.udev.packages = [udevRules];
    systemd.user.services = {
      scd = {
        description = "Steam Controller daemon";
        wantedBy = ["default.target"];
        unitConfig.ConditionPathExists = "%E/scd/config.toml";
        serviceConfig = {
          ExecStart = "${lib.getExe' cfg.package "scd"} --socket=%t/scd/control.sock";
          Restart = "on-failure";
          RestartSec = "1s";
          RuntimeDirectory = "scd";
          RuntimeDirectoryMode = "0700";
          UMask = "0077";
        };
      };
      scd-osk = lib.mkIf cfg.osk.enable {
        description = "Steam Controller on-screen keyboard";
        wantedBy = ["graphical-session.target"];
        partOf = ["graphical-session.target"];
        requires = ["scd.service"];
        after = ["scd.service"];
        unitConfig.ConditionPathExists = "%E/scd/config.toml";

        serviceConfig = {
          ExecStart = "${lib.getExe' cfg.package "scd-osk"} --socket=%t/scd/control.sock";
          Restart = "on-failure";
          RestartSec = "1s";
        };
      };
    };
  };
}
