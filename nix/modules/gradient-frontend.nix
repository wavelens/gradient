/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  gradientCfg = config.services.gradient;
  cfg = config.services.gradient.frontend;
in {
  options = {
    services.gradient.frontend = {
      enable = lib.mkEnableOption "Enable Gradient";
      package = lib.mkPackageOption pkgs "gradient-frontend" { };
      ip = lib.mkOption {
        description = "The IP address on which Gradient listens.";
        type = lib.types.str;
        default = "127.0.0.1";
      };

      port = lib.mkOption {
        description = "The port on which Gradient listens.";
        type = lib.types.int;
        default = 3001;
      };
    };
  };

  config = lib.mkIf (cfg.enable && gradientCfg.enable) {
    systemd.services.gradient-frontend = {
      wantedBy = [ "multi-user.target" ];
      after = [
        "network.target"
        "gradient-server.service"
      ];

      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
        StateDirectory = "gradient";
        DynamicUser = true;
        User = gradientCfg.user;
        Group = gradientCfg.group;
        ProtectHome = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProtectSystem = "strict";
        Restart = "on-failure";
        RestartSec = 10;
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
      };
    };
  };
}
