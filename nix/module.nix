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
      SUBSYSTEM=="misc", KERNEL=="uinput", TAG-="uaccess", OWNER:="root", GROUP:="uinput", MODE:="0660"
      SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="28de", ATTR{idProduct}=="1304", TAG-="uaccess", OWNER:="root", GROUP:="scd", MODE:="0660"
      SUBSYSTEM=="hidraw", KERNEL=="hidraw*", ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304", TAG-="uaccess", OWNER:="root", GROUP:="scd", MODE:="0660"
      SUBSYSTEM=="tty", ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304", TAG-="uaccess", OWNER:="root", GROUP:="root", MODE:="0000"
      SUBSYSTEM=="input", ATTRS{id/vendor}=="28de", ATTRS{id/product}=="1304", TAG-="uaccess", OWNER:="root", GROUP:="root", MODE:="0000", ENV{LIBINPUT_IGNORE_DEVICE}="1"
    '';
  };
in {
  options.services.scd = {
    enable = lib.mkEnableOption "the Steam Controller userspace daemon";

    configFile = lib.mkOption {
      type = lib.types.path;
      description = "Static TOML configuration for the daemon.";
    };

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

    osk.font = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Optional font file passed to the on-screen keyboard.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment = {
      etc."scd/config.toml" = {
        source = cfg.configFile;
        mode = "0444";
      };
      systemPackages = [cfg.package];
    };
    hardware.uinput.enable = true;
    services.udev.packages = [udevRules];
    users = {
      groups = {
        scd = {};
        scd-control = {};
      };
      users.scd = {
        isSystemUser = true;
        group = "scd";
      };
    };
    systemd = {
      services.scd = {
        description = "Steam Controller daemon";
        wantedBy = ["multi-user.target"];
        after = ["systemd-udevd.service"];
        restartTriggers = [cfg.configFile];
        serviceConfig = {
          ExecStart = "${lib.getExe' cfg.package "scd"} --config /etc/scd/config.toml --socket /run/scd/control.sock";
          User = "scd";
          Group = "scd-control";
          SupplementaryGroups = [
            "scd"
            "uinput"
          ];
          RuntimeDirectory = "scd";
          RuntimeDirectoryMode = "0770";
          UMask = "0007";
          Restart = "on-failure";
          RestartSec = "1s";
        };
      };
      user.services.scd-osk = lib.mkIf cfg.osk.enable {
        description = "Steam Controller on-screen keyboard";
        wantedBy = ["graphical-session.target"];
        partOf = ["graphical-session.target"];
        after = ["graphical-session.target"];
        serviceConfig = {
          ExecStart =
            "${lib.getExe' cfg.package "scd-osk"} --socket /run/scd/control.sock"
            + lib.optionalString (cfg.osk.font != null) " --font ${cfg.osk.font}";
          Restart = "on-failure";
          RestartSec = "1s";
        };
      };
    };
  };
}
