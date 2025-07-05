/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.services.gradient;
in {
  imports = [
    ./gradient-frontend.nix
  ];

  options = {
    services.gradient = {
      enable = lib.mkEnableOption "Enable Gradient";
      configureNginx = lib.mkEnableOption "Configure Nginx";
      configurePostgres = lib.mkEnableOption "Configure Postgres";
      package = lib.mkPackageOption pkgs "gradient-server" { };
      package_nix = lib.mkPackageOption pkgs "nix" { };
      package_git = lib.mkPackageOption pkgs "git" { };
      package_zstd = lib.mkPackageOption pkgs "zstd" { };
      serveCache = lib.mkEnableOption "Serve cache";
      reportErrors = lib.mkEnableOption "Report errors to Sentry";
      domain = lib.mkOption {
        description = "The domain under which Gradient runs.";
        type = lib.types.str;
        example = "gradient.example.com";
      };

      baseDir = lib.mkOption {
        description = "The base directory for Gradient.";
        type = lib.types.str;
        default = "/var/lib/gradient";
      };

      listenAddr = lib.mkOption {
        description = "The IP address on which Gradient listens.";
        type = lib.types.str;
        default = "127.0.0.1";
      };

      port = lib.mkOption {
        description = "The port on which Gradient listens.";
        type = lib.types.port;
        default = 3000;
      };

      jwtSecretFile = lib.mkOption {
        description = "The secret key file used to sign JWTs.";
        type = lib.types.str;
      };

      cryptSecretFile = lib.mkOption {
        description = "The base64-encoded secret key file.";
        type = lib.types.str;
      };

      databaseUrl = lib.mkOption {
        description = "The URL of the database to use.";
        type = lib.types.str;
        default = "postgresql://localhost/gradient?host=/run/postgresql";
      };

      databaseUrlFile = lib.mkOption {
        description = "The URL-file of the database to use.";
        type = lib.types.str;
        default = toString (pkgs.writeText "database_url" cfg.databaseUrl);
        example = "/etc/gradient/database_url";
      };

      oidc = {
        enable = lib.mkEnableOption "Enable OIDC";
        required = lib.mkEnableOption "Require OIDC for registration.";
        clientId = lib.mkOption {
          description = "The client ID for OIDC.";
          type = lib.types.str;
        };

        clientSecretFile = lib.mkOption {
          description = "The client secret file for OIDC.";
          type = lib.types.str;
        };

        scopes = lib.mkOption {
          description = "The scopes for OIDC.";
          type = lib.types.listOf lib.types.str;
          default = ["openid" "email" "profile"];
        };

        discoveryUrl = lib.mkOption {
          description = "The discovery URL for OIDC.";
          type = lib.types.str;
        };
      };

      settings = {
        disableRegistration = lib.mkEnableOption "Disable registration. Users must be registered via OIDC.";
        maxConcurrentEvaluations = lib.mkOption {
          description = "The maximum number of concurrent evaluations.";
          type = lib.types.ints.unsigned;
          default = 1;
        };

        maxConcurrentBuilds = lib.mkOption {
          description = "The maximum number of concurrent builds.";
          type = lib.types.ints.unsigned;
          default = 1;
        };

        logLevel = lib.mkOption {
          description = "The log level for the application.";
          type = lib.types.enum ["trace" "debug" "info" "warn" "error"];
          default = "info";
        };
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package_git ];
    systemd.services.gradient-server = {
      wantedBy = [ "multi-user.target" ];
      after = [
        "network.target"
        "postgresql.service"
      ];

      path = [
        cfg.package_nix
        cfg.package_git
      ];

      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
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
        WorkingDirectory = cfg.baseDir;
        LoadCredential = [
          "gradient_database_url:${cfg.databaseUrlFile}"
          "gradient_crypt_secret:${cfg.cryptSecretFile}"
          "gradient_jwt_secret:${cfg.jwtSecretFile}"
        ] ++ lib.optional cfg.oidc.enable [
          "gradient_oidc_client_secret:${cfg.oidc.clientSecretFile}"
        ];
      };

      environment = {
        NIX_REMOTE = "daemon";
        XDG_CACHE_HOME = "${cfg.baseDir}/www/.cache";
        GRADIENT_DEBUG = "false";
        GRADIENT_IP = cfg.listenAddr;
        GRADIENT_PORT = toString cfg.port;
        GRADIENT_SERVE_URL = "https://${cfg.domain}";
        GRADIENT_BASE_PATH = cfg.baseDir;
        GRADIENT_DATABASE_URL_FILE = "%d/gradient_database_url";
        GRADIENT_MAX_CONCURRENT_EVALUATIONS = toString cfg.settings.maxConcurrentEvaluations;
        GRADIENT_MAX_CONCURRENT_BUILDS = toString cfg.settings.maxConcurrentBuilds;
        GRADIENT_BINPATH_NIX = lib.getExe cfg.package_nix;
        GRADIENT_BINPATH_GIT = lib.getExe cfg.package_git;
        GRADIENT_BINPATH_ZSTD = lib.getExe cfg.package_zstd;
        GRADIENT_OIDC_ENABLED = lib.boolToString cfg.oidc.enable;
        GRADIENT_DISABLE_REGISTRATION = lib.boolToString cfg.settings.disableRegistration;
        GRADIENT_CRYPT_SECRET_FILE = "%d/gradient_crypt_secret";
        GRADIENT_JWT_SECRET_FILE = "%d/gradient_jwt_secret";
        GRADIENT_SERVE_CACHE = lib.boolToString cfg.serveCache;
        GRADIENT_REPORT_ERRORS = lib.boolToString cfg.reportErrors;
        GRADIENT_LOG_LEVEL = cfg.settings.logLevel;
        # Set RUST_LOG environment variable for enhanced logging
        RUST_LOG = cfg.settings.logLevel;
      } // lib.optionalAttrs cfg.oidc.enable {
        GRADIENT_OIDC_CLIENT_ID = cfg.oidc.clientId;
        GRADIENT_OIDC_CLIENT_SECRET_FILE = "%d/gradient_oidc_client_secret";
        GRADIENT_OIDC_SCOPES = builtins.concatStringsSep " " cfg.oidc.scopes;
        GRADIENT_OIDC_DISCOVERY_URL = cfg.oidc.discoveryUrl;
        GRADIENT_OIDC_REQUIRED = lib.boolToString cfg.oidc.required;
      };
    };

    nix.settings = {
      trusted-users = [ "gradient" ];
      experimental-features = [
        "nix-command"
        "flakes"
        "ca-derivations"
      ];
    };

    services = {
      nginx = lib.mkIf cfg.configureNginx {
        enable = true;
        virtualHosts."${cfg.domain}".locations = {
          "/" = lib.mkIf cfg.frontend.enable {
            proxyPass = "http://127.0.0.1:${toString config.services.gradient.frontend.port}";
            proxyWebsockets = true;
          };

          "/api" = {
            proxyPass = "http://127.0.0.1:${toString config.services.gradient.port}";
            proxyWebsockets = true;
          };

          "/cache" = lib.mkIf cfg.serveCache {
            proxyPass = "http://127.0.0.1:${toString config.services.gradient.port}";
            proxyWebsockets = true;
          };
        };
      };

      postgresql = lib.mkIf cfg.configurePostgres {
        enable = true;
        ensureDatabases = [ "gradient" ];
        ensureUsers = [{
          name = "gradient";
          ensureDBOwnership = true;
        }];
      };
    };

    users = {
      groups.gradient = { };
      users.gradient = {
        description = "Gradient user";
        isSystemUser = true;
        home = cfg.baseDir;
        createHome = true;
        group = "gradient";
      };
    };
  };
}
