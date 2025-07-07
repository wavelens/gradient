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
        default = "http://127.0.0.1:${toString gradientCfg.port}";
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

      preStart = ''
        ${lib.getExe cfg.package} migrate
      '';

      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package.python.pkgs.gunicorn} --bind ${cfg.listenAddr}:${toString cfg.port} --worker-tmp-dir /dev/shm frontend.wsgi:application";
        StateDirectory = "gradient";
        User = "gradient";
        Group = "gradient";
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
          "gradient_crypt_secret:${gradientCfg.cryptSecretFile}"
        ];
      };

      environment = {
        PYTHONPATH = "${cfg.package.python.pkgs.makePythonPath cfg.package.propagatedBuildInputs}:${cfg.package}/lib/gradient-frontend";
        GRADIENT_DEBUG = "false";
        GRADIENT_FRONTEND_IP = cfg.listenAddr;
        GRADIENT_FRONTEND_PORT = toString cfg.port;
        GRADIENT_API_URL = cfg.apiUrl;
        GRADIENT_SERVE_URL = "https://${gradientCfg.domain}";
        GRADIENT_BASE_PATH = gradientCfg.baseDir;
        GRADIENT_OIDC_ENABLE = lib.boolToString gradientCfg.oidc.enable;
        GRADIENT_DISABLE_REGISTRATION = lib.boolToString gradientCfg.settings.disableRegistration;
        GRADIENT_MAX_CONCURRENT_EVALUATIONS = toString gradientCfg.settings.maxConcurrentEvaluations;
        GRADIENT_MAX_CONCURRENT_BUILDS = toString gradientCfg.settings.maxConcurrentBuilds;
        GRADIENT_CRYPT_SECRET_FILE = "%d/gradient_crypt_secret";
        GRADIENT_SERVE_CACHE = lib.boolToString gradientCfg.serveCache;
        GRADIENT_REPORT_ERRORS = lib.boolToString gradientCfg.reportErrors;
      } // lib.optionalAttrs gradientCfg.oidc.enable {
        GRADIENT_OIDC_REQUIRED = lib.boolToString gradientCfg.oidc.required;
      } // lib.optionalAttrs gradientCfg.email.enable {
        GRADIENT_EMAIL_ENABLED = lib.boolToString gradientCfg.email.enable;
        GRADIENT_EMAIL_REQUIRE_VERIFICATION = lib.boolToString gradientCfg.email.requireVerification;
      };
    };
  };
}
