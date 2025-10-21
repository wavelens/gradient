/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.services.gradient;

  stateJsonFile = pkgs.writeText "gradient-state.json" (builtins.toJSON cfg.state);
  userPasswordFiles = map (user: "gradient_user_${user.username}_password:${user.password_file}") cfg.state.users;
  orgPrivateKeyFiles = map (org: "gradient_org_${org.name}_private_key:${org.private_key_file}") cfg.state.organizations;
  cacheSigningKeyFiles = map (cache: "gradient_cache_${cache.name}_signing_key:${cache.signing_key_file}") cfg.state.caches;
  apiKeyFiles = map (api_key: "gradient_api_${api_key.name}_key:${api_key.key_file}") cfg.state.api_keys;
in {
  imports = [
    ./gradient-state.nix
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
      package_ssh = lib.mkPackageOption pkgs "openssh" { };
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

      email = {
        enable = lib.mkEnableOption "Enable email functionality";
        requireVerification = lib.mkEnableOption "Require email verification for new registrations";
        smtpHost = lib.mkOption {
          description = "SMTP server hostname";
          type = lib.types.str;
        };

        smtpPort = lib.mkOption {
          description = "SMTP server port";
          type = lib.types.port;
          default = 587;
        };

        smtpUsername = lib.mkOption {
          description = "SMTP username";
          type = lib.types.str;
        };

        smtpPasswordFile = lib.mkOption {
          description = "File containing SMTP password";
          type = lib.types.str;
        };

        fromAddress = lib.mkOption {
          description = "Email address to send from";
          type = lib.types.str;
        };

        fromName = lib.mkOption {
          description = "Name to display in email from field";
          type = lib.types.str;
          default = "Gradient";
        };

        disableTls = lib.mkOption {
          description = "Disable TLS for SMTP connections (useful for testing)";
          type = lib.types.bool;
          default = false;
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
          type = lib.types.enum [ "trace" "debug" "info" "warn" "error" ];
          default = "info";
        };

        deleteState = lib.mkOption {
          description = "Delete all state (users, organizations, caches) if not manged anymore by state (instead of making editable by user in frontend).";
          type = lib.types.bool;
          default = true;
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
        "postgresql.target"
      ];

      path = [
        cfg.package_nix
        cfg.package_git
        cfg.package_ssh
      ];

      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
        StateDirectory = "gradient";
        User = "gradient";
        Group = "gradient";
        PrivateTmp = true;
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
          "gradient_state:${stateJsonFile}"
        ] ++ lib.optional cfg.oidc.enable [
          "gradient_oidc_client_secret:${cfg.oidc.clientSecretFile}"
        ] ++ lib.optional cfg.email.enable [
          "gradient_email_smtp_password:${cfg.email.smtpPasswordFile}"
        ] ++ userPasswordFiles ++ orgPrivateKeyFiles ++ cacheSigningKeyFiles ++ apiKeyFiles;
      };

      environment = {
        NIX_REMOTE = "daemon";
        XDG_CACHE_HOME = "${cfg.baseDir}/www/.cache";
        GRADIENT_IP = cfg.listenAddr;
        GRADIENT_PORT = toString cfg.port;
        GRADIENT_SERVE_URL = "https://${cfg.domain}";
        GRADIENT_BASE_PATH = cfg.baseDir;
        GRADIENT_DATABASE_URL_FILE = "%d/gradient_database_url";
        GRADIENT_MAX_CONCURRENT_EVALUATIONS = toString cfg.settings.maxConcurrentEvaluations;
        GRADIENT_MAX_CONCURRENT_BUILDS = toString cfg.settings.maxConcurrentBuilds;
        GRADIENT_BINPATH_NIX = lib.getExe cfg.package_nix;
        GRADIENT_BINPATH_GIT = lib.getExe cfg.package_git;
        GRADIENT_BINPATH_SSH = lib.getExe' cfg.package_ssh "ssh";
        GRADIENT_OIDC_ENABLED = lib.boolToString cfg.oidc.enable;
        GRADIENT_DISABLE_REGISTRATION = lib.boolToString cfg.settings.disableRegistration;
        GRADIENT_CRYPT_SECRET_FILE = "%d/gradient_crypt_secret";
        GRADIENT_JWT_SECRET_FILE = "%d/gradient_jwt_secret";
        GRADIENT_SERVE_CACHE = lib.boolToString cfg.serveCache;
        GRADIENT_REPORT_ERRORS = lib.boolToString cfg.reportErrors;
        GRADIENT_LOG_LEVEL = cfg.settings.logLevel;
        GRADIENT_DELETE_STATE = lib.boolToString cfg.settings.deleteState;
        GRADIENT_STATE_FILE = "%d/gradient_state";
        GRADIENT_CREDENTIALS_DIR = "%d";
        RUST_LOG = cfg.settings.logLevel;
      } // lib.optionalAttrs cfg.oidc.enable {
        GRADIENT_OIDC_CLIENT_ID = cfg.oidc.clientId;
        GRADIENT_OIDC_CLIENT_SECRET_FILE = "%d/gradient_oidc_client_secret";
        GRADIENT_OIDC_SCOPES = builtins.concatStringsSep " " cfg.oidc.scopes;
        GRADIENT_OIDC_DISCOVERY_URL = cfg.oidc.discoveryUrl;
        GRADIENT_OIDC_REQUIRED = lib.boolToString cfg.oidc.required;
      } // lib.optionalAttrs cfg.email.enable {
        GRADIENT_EMAIL_ENABLED = lib.boolToString cfg.email.enable;
        GRADIENT_EMAIL_REQUIRE_VERIFICATION = lib.boolToString cfg.email.requireVerification;
        GRADIENT_EMAIL_SMTP_HOST = cfg.email.smtpHost;
        GRADIENT_EMAIL_SMTP_PORT = toString cfg.email.smtpPort;
        GRADIENT_EMAIL_SMTP_USERNAME = cfg.email.smtpUsername;
        GRADIENT_EMAIL_SMTP_PASSWORD_FILE = "%d/gradient_email_smtp_password";
        GRADIENT_EMAIL_FROM_ADDRESS = cfg.email.fromAddress;
        GRADIENT_EMAIL_FROM_NAME = cfg.email.fromName;
        GRADIENT_EMAIL_DISABLE_TLS = lib.boolToString cfg.email.disableTls;
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

          "/api/" = {
            proxyPass = "http://127.0.0.1:${toString config.services.gradient.port}";
            proxyWebsockets = true;
          };

          "/cache/" = lib.mkIf cfg.serveCache {
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
