{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.scd;
  udevRules = pkgs.writeTextDir "lib/udev/rules.d/72-scd.rules" ''
    # expose the emulated wired controller to the graphical session
    SUBSYSTEM=="hidraw", KERNEL=="hidraw*", KERNELS=="0003:054C:09CC.*", TAG+="uaccess", GROUP:="input", MODE:="0660"
    # allow scd to claim the physical receiver while it runs
    SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="28de", ATTR{idProduct}=="1304", TAG-="uaccess", GROUP:="scd", MODE:="0660"
  '';
  # uaccess runs at 73, so clear stale receiver ACLs afterward
  aclRules = pkgs.writeTextDir "lib/udev/rules.d/74-scd.rules" ''
    ACTION!="remove", SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ENV{DEVNAME}!="", ATTR{idVendor}=="28de", ATTR{idProduct}=="1304", RUN{program}+="${lib.getExe' pkgs.acl "setfacl"} --remove-all $env{DEVNAME}"
  '';
in {
  options.services.scd = {
    enable = lib.mkEnableOption "the Steam Controller userspace daemon";
    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix {};
      description = "The scd package to use.";
    };
    configFile = lib.mkOption {
      type = lib.types.path;
      description = "The Steam Controller configuration file.";
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
    # provide the virtual usb host used by wired dualshock emulation
    boot.kernelModules = ["vhci_hcd"];
    hardware.uinput.enable = true;
    services.udev.packages = [
      udevRules
      aclRules
    ];
    users = {
      groups = {
        scd = {};
        # permit user clients to access the daemon socket
        scd-control = {};
      };
      users.scd = {
        isSystemUser = true;
        group = "scd-control";
      };
    };
    systemd = {
      services.scd = {
        description = "Steam Controller daemon";
        wantedBy = ["multi-user.target"];
        after = [
          "systemd-modules-load.service"
          "systemd-udevd.service"
        ];
        serviceConfig = {
          ExecStartPre = [
            # grant the daemon access to attach and detach virtual usb devices
            "+${pkgs.coreutils}/bin/chgrp scd /sys/devices/platform/vhci_hcd.0/attach /sys/devices/platform/vhci_hcd.0/detach"
            "+${pkgs.coreutils}/bin/chmod 0660 /sys/devices/platform/vhci_hcd.0/attach /sys/devices/platform/vhci_hcd.0/detach"
            # reapply receiver permissions when it was connected before startup
            "+${pkgs.systemd}/bin/udevadm trigger --settle --action=change --property-match=ID_VENDOR_ID=28de --property-match=ID_MODEL_ID=1304"
          ];
          ExecStart = "${lib.getExe' cfg.package "scd"} --config ${cfg.configFile} --socket /run/scd/control.sock";
          ExecStopPost = [
            # revoke virtual usb control after shutdown
            "+${pkgs.coreutils}/bin/chgrp root /sys/devices/platform/vhci_hcd.0/attach /sys/devices/platform/vhci_hcd.0/detach"
            "+${pkgs.coreutils}/bin/chmod 0200 /sys/devices/platform/vhci_hcd.0/attach /sys/devices/platform/vhci_hcd.0/detach"
          ];
          User = "scd";
          SupplementaryGroups = [
            "scd"
            "uinput"
          ];
          RuntimeDirectory = "scd";
          RuntimeDirectoryMode = "0770";
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
