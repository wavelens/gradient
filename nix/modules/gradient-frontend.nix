/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  gradientCfg = config.services.gradient;
  cfg = config.services.gradient.frontend;
in {
  options = {
    services.gradient.frontend = {
      enable = lib.mkEnableOption "Enable Gradient Frontend";
      package = lib.mkPackageOption pkgs "gradient-frontend" { };
      listenAddr = lib.mkOption {
        description = "The IP address on which Gradient listens.";
        type = lib.types.str;
        default = gradientCfg.listenAddr;
      };

      port = lib.mkOption {
        description = "The port on which Gradient listens.";
        type = lib.types.port;
        default = 3001;
      };

      apiUrl = lib.mkOption {
        description = "The URL of the Gradient API.";
        type = lib.types.str;
        default = "http://127.0.0.1:${gradientCfg.port}";
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
        LoadCredential = [
          "GRADIENT_CRYPT_SECRET:${gradientCfg.cryptSecretFile}"
        ];
      };

      environment = {
        GRADIENT_DEBUG = "false";
        GRADIENT_FRONTEND_IP = cfg.listenAddr;
        GRADIENT_FRONTEND_PORT = toString cfg.port;
        GRADIENT_API_URL = cfg.apiUrl;
        GRADIENT_SERVE_URL = "https://${gradientCfg.domain}";
      } // lib.optionalAttrs gradientCfg.oauth.enable {
        GRADIENT_OAUTH_REQUIRED = toString gradientCfg.settings.disableRegistration;
      };
    };
  };
}
