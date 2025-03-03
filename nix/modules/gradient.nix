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
      serveCache = lib.mkEnableOption "Serve cache";
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

      oauth = {
        enable = lib.mkEnableOption "Enable OAuth";
        required = lib.mkEnableOption "Require OAuth for registration.";
        clientId = lib.mkOption {
          description = "The client ID for OAuth.";
          type = lib.types.str;
        };

        clientSecretFile = lib.mkOption {
          description = "The client secret file for OAuth.";
          type = lib.types.str;
        };

        scopes = lib.mkOption {
          description = "The scopes for OAuth.";
          type = lib.types.listOf lib.types.str;
          default = [];
        };

        tokenUrl = lib.mkOption {
          description = "The token URL for OAuth.";
          type = lib.types.str;
        };

        authUrl = lib.mkOption {
          description = "The auth URL for OAuth.";
          type = lib.types.str;
        };

        apiUrl = lib.mkOption {
          description = "The API URL for OAuth.";
          type = lib.types.str;
        };
      };

      settings = {
        disableRegistration = lib.mkEnableOption "Disable registration. Users must be registered via OAuth2.";
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
      };
    };
  };

  config = lib.mkIf cfg.enable {
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
          "gradient_crypt_secret:${cfg.cryptSecretFile}"
          "gradient_jwt_secret:${cfg.jwtSecretFile}"
        ] ++ lib.optional cfg.oauth.enable [
          "gradient_oauth_client_secret:${cfg.oauth.clientSecretFile}"
        ];
      };

      environment = {
        XDG_CACHE_HOME = "${cfg.baseDir}/www/.cache";
        GRADIENT_DEBUG = "false";
        GRADIENT_IP = cfg.listenAddr;
        GRADIENT_PORT = toString cfg.port;
        GRADIENT_SERVE_URL = "https://${cfg.domain}";
        GRADIENT_BASE_PATH = cfg.baseDir;
        GRADIENT_DATABASE_URL = cfg.databaseUrl;
        GRADIENT_MAX_CONCURRENT_EVALUATIONS = toString cfg.settings.maxConcurrentEvaluations;
        GRADIENT_MAX_CONCURRENT_BUILDS = toString cfg.settings.maxConcurrentBuilds;
        GRADIENT_BINPATH_NIX = lib.getExe cfg.package_nix;
        GRADIENT_BINPATH_GIT = lib.getExe cfg.package_git;
        GRADIENT_OAUTH_ENABLE = lib.boolToString cfg.oauth.enable;
        GRADIENT_DISABLE_REGISTER = lib.boolToString cfg.settings.disableRegistration;
        GRADIENT_CRYPT_SECRET_FILE = "%d/gradient_crypt_secret";
        GRADIENT_JWT_SECRET_FILE = "%d/gradient_jwt_secret";
        GRADIENT_SERVE_CACHE = lib.boolToString cfg.serveCache;
      } // lib.optionalAttrs cfg.oauth.enable {
        GRADIENT_OAUTH_CLIENT_ID = cfg.oauth.clientId;
        GRADIENT_OAUTH_CLIENT_SECRET_FILE = "%d/gradient_oauth_client_secret";
        GRADIENT_OAUTH_SCOPES = builtins.concatStringsSep " " cfg.oauth.scopes;
        GRADIENT_OAUTH_TOKEN_URL = cfg.oauth.tokenUrl;
        GRADIENT_OAUTH_AUTH_URL = cfg.oauth.authUrl;
        GRADIENT_OAUTH_API_URL = cfg.oauth.apiUrl;
        GRADIENT_OAUTH_REQUIRED = lib.boolToString cfg.oauth.required;
      };
    };

    nix.settings = {
      allowed-users = [ "gradient" ];
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

    users.users.gradient = {
      description = "Gradient user";
      isSystemUser = true;
      home = cfg.baseDir;
      createHome = true;
      group = "gradient";
    };

    users.groups.gradient = { };
  };
}
