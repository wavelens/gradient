/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.gradient.guest;
  inherit (config.boot.kernelPackages) kernel usbip;
  toplevel = config.system.build.toplevel;

  kernelImage = if pkgs.stdenv.hostPlatform.isx86_64 then "${kernel.dev}/vmlinux" else "${kernel}/Image";
  initrd = "${config.system.build.initialRamdisk}/${config.system.boot.loader.initrdFile}";

  guestJson = pkgs.writeText "guest.json" (builtins.toJSON {
    inherit (cfg) cpus;
    memory_mb = cfg.memoryMb;
    cmdline = lib.concatStringsSep " " ([ "init=${toplevel}/init" ] ++ config.boot.kernelParams);
  });

  script = text: {
    mode = "0755";
    text = ''
      #!${pkgs.runtimeShell}
      ${text}
    '';
  };

  agentCommand = name: command: pkgs.writeShellScriptBin name ''
    exec ${cfg.package}/bin/gradient-guest-agent ${command} "$@"
  '';
in {
  options.gradient.guest = {
    enable = lib.mkEnableOption "the guest system of a Gradient exec worker image, with the guest agent";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.gradient.guest;
      defaultText = lib.literalExpression "pkgs.gradient.guest";
      description = "Package that provides {command}`gradient-guest-agent`.";
    };

    preScript = lib.mkOption {
      type = lib.types.lines;
      default = "";
      example = "nvidia-smi";
      description = ''
        Shell script run as root before each exec job's program, once the job context,
        secrets and mounts are in place. A failing script fails the job.
      '';
    };

    postScript = lib.mkOption {
      type = lib.types.lines;
      default = "";
      description = ''
        Shell script run as root after each exec job's program, also when the program failed.
      '';
    };

    cpus = lib.mkOption {
      type = lib.types.ints.between 1 255;
      default = 2;
      description = ''
        Warm guests boot from this image with this many virtual CPUs. Exec jobs only take a warm guest when the job's `cpus` and `memoryMb` equal these values.
      '';
    };

    memoryMb = lib.mkOption {
      type = lib.types.ints.positive;
      default = 500;
      description = "Memory in MiB of each guest booted from this image.";
    };
  };

  config = lib.mkIf cfg.enable {
    boot = {
      loader.grub.enable = false;
      kernelParams = [ "console=ttyS0" ];
      kernelModules = [ "vhci-hcd" "virtio_blk" "ext4" ];
      initrd.kernelModules = [ "virtio_pci" "virtiofs" "vsock" "vmw_vsock_virtio_transport" ];
    };

    fileSystems = {
      "/" = {
        device = "none";
        fsType = "tmpfs";
        options = [ "mode=0755" ];
      };

      "/nix/store" = {
        device = "nix-store";
        fsType = "virtiofs";
        options = [ "ro" ];
        neededForBoot = true;
      };
    };

    networking.hostName = lib.mkDefault "gradient";
    networking.useDHCP = false;
    networking.usePredictableInterfaceNames = false;
    networking.firewall.enable = lib.mkDefault false;
    documentation.enable = false;
    system.stateVersion = lib.mkDefault lib.trivial.release;

    users = {
      groups = { gradient.gid = 1000; kvm = { }; dialout = { }; video = { }; render = { }; };
      users.gradient = {
        isNormalUser = true;
        uid = 1000;
        group = "gradient";
        home = "/home/gradient";
        description = "User for the Gradient exec job";
      };
    };

    services.getty.autologinUser = lib.mkDefault "gradient";
    systemd.services."serial-getty@ttyS0".environment.TERM = "xterm-256color";

    services.udev.extraRules = ''
      SUBSYSTEM=="usb", MODE="0660", GROUP="dialout"
    '';

    environment = {
      etc."gradient/pre-script" = script cfg.preScript;
      etc."gradient/post-script" = script cfg.postScript;
      systemPackages = [
        usbip
        pkgs.git
        pkgs.gradient-cli
        (agentCommand "gradient-phase" "phase")
        (agentCommand "gradient-upload" "upload")
      ];
    };

    systemd.services.gradient-guest-agent = {
      description = "Guest Agent from Gradient";
      wantedBy = [ "multi-user.target" ];
      path = [ pkgs.git pkgs.iproute2 pkgs.util-linux usbip ];
      serviceConfig = {
        ExecStart = lib.getExe' cfg.package "gradient-guest-agent";
        Restart = "always";
      };
    };

    system.build.gradientGuest = pkgs.runCommand "gradient-guest" { } ''
      mkdir $out
      ln -s ${kernelImage} $out/kernel
      ln -s ${initrd} $out/initrd
      ln -s ${toplevel} $out/toplevel
      cp ${guestJson} $out/guest.json
    '';
  };
}
