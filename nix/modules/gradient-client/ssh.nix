/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.nix.gradient-ssh;
in {
  options.nix.gradient-ssh = {
    enable = lib.mkEnableOption "Gradient as a remote builder over SSH";

    host = lib.mkOption {
      type = lib.types.str;
      example = "gradient.example.com";
      description = "Host name of the Gradient server.";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 2222;
      description = "Port of the Gradient SSH server, see {option}`services.gradient.ssh.port`.";
    };

    project = lib.mkOption {
      type = lib.types.str;
      example = "my-project";
      description = "Project the builds are running in. It is the SSH user name.";
    };

    identityFile = lib.mkOption {
      type = lib.types.str;
      example = "/run/secrets/gradient-ssh-key";
      description = ''
        File containing the SSH private key. Its public key must be registered in Gradient for a
        user with the `TriggerEvaluation` permission in {option}`nix.gradient-ssh.project`.
      '';
    };

    systems = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ pkgs.stdenv.hostPlatform.system ];
      defaultText = lib.literalExpression "[ pkgs.stdenv.hostPlatform.system ]";
      example = [ "x86_64-linux" "aarch64-linux" ];
      description = "Systems the Gradient workers are building for.";
    };

    supportedFeatures = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "benchmark" "big-parallel" "kvm" "nixos-test" ];
      description = "System features the Gradient workers are offering.";
    };

    maxJobs = lib.mkOption {
      type = lib.types.ints.positive;
      default = 128;
      description = "Number of builds Nix is sending to Gradient at once.";
    };

    speedFactor = lib.mkOption {
      type = lib.types.ints.positive;
      default = 128;
      description = "Preference of Gradient over other build machines. A higher value is preferring Gradient.";
    };
  };

  config = lib.mkIf cfg.enable {
    nix = {
      distributedBuilds = true;
      settings.builders-use-substitutes = true;
      buildMachines = [{
        hostName = cfg.host;
        protocol = "ssh-ng";
        inherit (cfg) maxJobs speedFactor systems supportedFeatures;
      }];
    };

    programs.ssh.extraConfig = ''
      Host ${cfg.host}
        User ${cfg.project}
        Port ${toString cfg.port}
        IdentityFile ${cfg.identityFile}
        IPQoS cs0
    '';
  };
}
