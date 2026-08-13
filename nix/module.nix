{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.scd;
  udevRules = pkgs.writeTextDir "lib/udev/rules.d/72-scd.rules" ''
    KERNEL=="uhid", OWNER:="root", GROUP:="scd", MODE:="0660"
    SUBSYSTEM=="hidraw", KERNEL=="hidraw*", KERNELS=="0005:054C:0CE6.*", TAG+="uaccess", GROUP:="input", MODE:="0660"
    SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="28de", ATTR{idProduct}=="1304", TAG-="uaccess", OWNER:="scd", GROUP:="scd-control", MODE:="0600"
    TEST!="/run/scd/steam-device", SUBSYSTEM=="hidraw", KERNEL=="hidraw*", ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304", OWNER:="root", GROUP:="scd", MODE:="0660"
    TEST!="/run/scd/steam-device", SUBSYSTEM=="tty", ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304", OWNER:="root", GROUP:="root", MODE:="0000"
    TEST!="/run/scd/steam-device", SUBSYSTEM=="input", ATTRS{id/vendor}=="28de", ATTRS{id/product}=="1304", OWNER:="root", GROUP:="root", MODE:="0000", ENV{LIBINPUT_IGNORE_DEVICE}="1"
  '';
  # uaccess runs at 73, so clear any stale puck ACL afterward
  aclRules = pkgs.writeTextDir "lib/udev/rules.d/74-scd.rules" ''
    ACTION!="remove", TEST!="/run/scd/steam-device", SUBSYSTEMS=="usb", ATTRS{idVendor}=="28de", ATTRS{idProduct}=="1304", ENV{DEVNAME}!="", TAG-="uaccess", RUN{program}+="${lib.getExe' pkgs.acl "setfacl"} --remove-all $env{DEVNAME}"
  '';
in {
  options.services.scd = {
    enable = lib.mkEnableOption "the Steam Controller userspace daemon";
    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix {};
      description = "The scd package to use.";
    };
    osk = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Whether to run the on-screen keyboard in the graphical session.";
      };
      font = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = "${pkgs.jetbrains-mono}/share/fonts/truetype/JetBrainsMono[wght].ttf";
        description = "Font file passed to the on-screen keyboard, or null to use its configuration.";
      };
    };
  };
  config = lib.mkIf cfg.enable {
    environment.systemPackages = [cfg.package];
    boot.kernelModules = ["uhid"];
    hardware.uinput.enable = true;
    services.udev.packages = [
      udevRules
      aclRules
    ];
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
        serviceConfig = {
          ExecStartPre = "+${pkgs.systemd}/bin/udevadm trigger --settle --action=change --property-match=ID_VENDOR_ID=28de --property-match=ID_MODEL_ID=1304";
          ExecStart = "${lib.getExe' cfg.package "scd"} --socket /run/scd/control.sock";
          User = "scd";
          Group = "scd-control";
          SupplementaryGroups = [
            "scd"
            "uinput"
          ];
          RuntimeDirectory = "scd";
          RuntimeDirectoryPreserve = "yes";
          RuntimeDirectoryMode = "0770";
          UMask = "0007";
          Restart = "on-failure";
          RestartSec = "1s";
        };
      };
      user.paths.scd-config = {
        description = "Watch the Steam Controller configuration";
        wantedBy = [
          "graphical-session.target"
          "nixos-activation.service"
        ];
        partOf = ["graphical-session.target"];
        pathConfig.PathChanged = [
          "%E/scd/config.toml"
          "/run/scd/control.sock"
        ];
      };
      user.services.scd-config = {
        description = "Apply the Steam Controller configuration";
        wantedBy = [
          "graphical-session.target"
          "nixos-activation.service"
        ];
        partOf = ["graphical-session.target"];
        after = ["nixos-activation.service"];
        unitConfig.ConditionPathExists = "%E/scd/config.toml";
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${lib.getExe' cfg.package "scctl"} config apply %E/scd/config.toml";
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
