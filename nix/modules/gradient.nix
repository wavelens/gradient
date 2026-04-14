/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.services.gradient;
  logLevelType = lib.types.enum [ "trace" "debug" "info" "warn" "error" ];
  augmentedProjects = lib.mapAttrs (_: proj: proj // {
    ci_reporter_has_token = proj.ci_reporter_token_file != null;
  }) cfg.state.projects;

  stateJsonFile = pkgs.writers.writeJSON "gradient-state.json" (cfg.state // {
    projects = augmentedProjects;
  });

  userPasswordFiles = lib.mapAttrsToList (_: user: "gradient_user_${user.username}_password:${user.password_file}") cfg.state.users;
  orgPrivateKeyFiles = lib.mapAttrsToList (_: org: "gradient_org_${org.name}_private_key:${org.private_key_file}") cfg.state.organizations;
  cacheSigningKeyFiles = lib.mapAttrsToList (_: cache: "gradient_cache_${cache.name}_signing_key:${cache.signing_key_file}") cfg.state.caches;
  apiKeyFiles = lib.mapAttrsToList (_: api_key: "gradient_api_${api_key.name}_key:${api_key.key_file}") cfg.state.api_keys;
  workerTokenFiles = lib.mapAttrsToList (_: worker: "gradient_worker_${worker.worker_id}_token:${worker.token_file}") cfg.state.workers;
  projectCiTokenFiles = lib.concatLists (lib.mapAttrsToList (_: proj:
    lib.optional (proj.ci_reporter_token_file != null)
      "gradient_project_${proj.name}_ci_token:${proj.ci_reporter_token_file}"
  ) cfg.state.projects);
in {
  # disabledModules = [
  #   "services/gradient/default.nix"
  #   "services/gradient/worker.nix"
  #   "services/gradient/state.nix"
  # ];

  imports = [
    ./gradient-state.nix
    ./gradient-worker.nix
  ];

  options = {
    services.gradient = {
      enable = lib.mkEnableOption "Gradient";
      configureNginx = lib.mkEnableOption "Nginx configuration";
      configurePostgres = lib.mkEnableOption "PostgreSQL configuration";
      serveCache = lib.mkEnableOption "cache serving";
      reportErrors = lib.mkEnableOption "error reporting to Sentry";
      useTls = lib.mkEnableOption "TLS" // { default = true; };
      enableQuic = lib.mkEnableOption "Quic support";
      discoverable = lib.mkEnableOption "discoverable — accept incoming connections on /proto" // { default = true; };
      packages = {
        server = lib.mkPackageOption pkgs "gradient" { };
        frontend = lib.mkPackageOption pkgs "gradient-frontend" { };
        nix = lib.mkOption {
          default = config.nix.package;
          defaultText = lib.literalExpression "config.nix.package";
          type = lib.types.package;
          description = "Nix package to use";
        };

        ssh = lib.mkOption {
          default = config.programs.ssh.package;
          defaultText = lib.literalExpression "config.programs.ssh.package";
          type = lib.types.package;
          description = "OpenSSH package to use";
        };

        git = lib.mkOption {
          default = config.programs.git.package;
          defaultText = lib.literalExpression "config.programs.git.package";
          type = lib.types.package;
          description = "Git package to use";
        };
      };

      domain = lib.mkOption {
        description = "Domain under which Gradient is being served";
        type = lib.types.str;
        example = "gradient.example.com";
      };

      baseDir = lib.mkOption {
        description = "Base directory for Gradient";
        type = lib.types.path;
        default = "/var/lib/gradient";
      };

      listenAddr = lib.mkOption {
        description = "IP address on which Gradient listens";
        type = lib.types.str;
        default = "127.0.0.1";
      };

      port = lib.mkOption {
        description = "Port on which Gradient listens";
        type = lib.types.port;
        default = 3000;
      };

      jwtSecretFile = lib.mkOption {
        description = "Secret key file used to sign JWTs";
        type = lib.types.path;
      };

      cryptSecretFile = lib.mkOption {
        description = "Database encryption password file";
        type = lib.types.path;
      };

      databaseUrl = lib.mkOption {
        description = "URL of the database to use";
        type = lib.types.str;
        default = "postgresql://localhost/gradient?host=/run/postgresql";
      };

      databaseUrlFile = lib.mkOption {
        description = "URL-file of the database to use";
        type = lib.types.path;
        default = pkgs.writeText "database_url" cfg.databaseUrl;
        defaultText = lib.literalExpression "pkgs.writeText \"database_url\" config.services.gradient.databaseUrl;";
        example = "/etc/gradient/database_url";
      };

      proto = {
        federate = lib.mkEnableOption "federate Gradient Proto";
      };

      frontend = {
        enable = lib.mkEnableOption "Gradient Frontend";
        url = lib.mkOption {
          description = "Public URL of the Gradient frontend, used in CI status report links";
          type = lib.types.str;
          default = "http${lib.optionalString cfg.useTls "s"}://${cfg.domain}";
          defaultText = lib.literalExpression ''http''${lib.optionalString config.services.gradient.useTls "s"}://''${config.services.gradient.domain}'';
          example = "https://gradient.example.com";
        };
      };

      oidc = {
        enable = lib.mkEnableOption "OIDC";
        required = lib.mkEnableOption "OIDC requirement for registration";
        clientId = lib.mkOption {
          description = "Client ID for OIDC";
          type = lib.types.str;
        };

        clientSecretFile = lib.mkOption {
          description = "Client secret file for OIDC";
          type = lib.types.path;
        };

        scopes = lib.mkOption {
          description = "Scopes for OIDC";
          type = lib.types.listOf lib.types.str;
          default = ["openid" "email" "profile"];
        };

        discoveryUrl = lib.mkOption {
          description = "Discovery URL for OIDC";
          type = lib.types.str;
        };

        iconUrl = lib.mkOption {
          description = "Icon URL for OIDC provider";
          type = lib.types.nullOr lib.types.str;
          default = null;
        };
      };

      email = {
        enable = lib.mkEnableOption "email functionality";
        requireVerification = lib.mkEnableOption "email verification requirement for registrations";
        enableTls = lib.mkEnableOption "TLS for SMTP connections";
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
          type = lib.types.path;
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
      };

      githubApp = {
        enable = lib.mkEnableOption "GitHub App integration for webhook-triggered evaluations and CI reporting";
        appId = lib.mkOption {
          description = "GitHub App ID shown on the GitHub App settings page";
          type = lib.types.ints.positive;
        };

        privateKeyFile = lib.mkOption {
          description = "Path to the GitHub App RS256 private key PEM file";
          type = lib.types.path;
        };

        webhookSecretFile = lib.mkOption {
          description = ''
            File containing the shared secret used to verify incoming GitHub
            App webhook payloads. Must match the value configured on the
            GitHub App's webhook settings page.
          '';
          type = lib.types.path;
        };
      };

      s3 = {
        enable = lib.mkEnableOption "S3 storage for NAR cache files";
        bucket = lib.mkOption {
          description = "S3 bucket name for NAR cache storage";
          type = lib.types.str;
          default = "";
        };

        region = lib.mkOption {
          description = "AWS region for the S3 bucket";
          type = lib.types.str;
          default = "us-east-1";
        };

        endpoint = lib.mkOption {
          description = "Custom S3-compatible endpoint URL (e.g. for MinIO or Cloudflare R2). Null uses the default AWS endpoint";
          type = lib.types.nullOr lib.types.str;
          default = null;
        };

        accessKeyId = lib.mkOption {
          description = "AWS access key ID. Null falls back to instance credentials or environment variables";
          type = lib.types.nullOr lib.types.str;
          default = null;
        };

        secretAccessKeyFile = lib.mkOption {
          description = "File containing the AWS secret access key. Null falls back to instance credentials";
          type = lib.types.nullOr lib.types.path;
          default = null;
        };

        prefix = lib.mkOption {
          description = "Key prefix within the S3 bucket (e.g. \"gradient/\"). Leave empty to store at the bucket root";
          type = lib.types.str;
          default = "";
        };
      };

      settings = {
        enableRegistration = lib.mkEnableOption "registration. Users must be registered via OIDC." // { default = true; };
        maxProtoConnections = lib.mkOption {
          description = "Maximum number of simultaneous proto WebSocket connections";
          type = lib.types.ints.positive;
          default = 256;
        };

        keepEvaluations = lib.mkOption {
          description = "Amount of evaluations to keep in the database and cache";
          type = lib.types.ints.positive;
          default = 5;
        };

        logLevel = lib.mkOption {
          default = { };
          description = ''
            Log levels. `default` is the global level; `cache`, `web` and
            `proto` override per component (null inherits from `default`).
          '';

          type = lib.types.submodule {
            options = {
              default = lib.mkOption {
                description = "Default log level for the application";
                type = logLevelType;
                default = "info";
              };

              cache = lib.mkOption {
                description = "Log level for the cache service. Null inherits from default";
                type = lib.types.nullOr logLevelType;
                default = null;
              };

              web = lib.mkOption {
                description = "Log level for the web service. Null inherits from default";
                type = lib.types.nullOr logLevelType;
                default = null;
              };

              proto = lib.mkOption {
                description = "Log level for the protocol layer. Null inherits from default";
                type = lib.types.nullOr logLevelType;
                default = null;
              };
            };
          };
        };

        deleteState = lib.mkOption {
          description = "Delete all state (users, organizations, caches) if not manged anymore by state";
          type = lib.types.bool;
          default = true;
        };

        cacheTtlHours = lib.mkOption {
          description = "TTL in hours for cached NAR files that have not been fetched recently. 0 disables TTL-based GC";
          type = lib.types.ints.unsigned;
          default = 336;
        };
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.proto.federate -> cfg.discoverable;
        message = "proto.federate requires discoverable to be enabled";
      }
    ];

    systemd.services.gradient-server = {
      wantedBy = [ "multi-user.target" ];
      after = [
        "network.target"
        "systemd-tmpfiles-setup.service"
      ] ++ lib.optional cfg.configurePostgres "postgresql.target";

      path = [
        cfg.packages.nix
        cfg.packages.ssh
        cfg.packages.git
      ];

      serviceConfig = {
        ExecStart = lib.getExe cfg.packages.server;
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
        LimitNOFILE = 65535;
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
        ] ++ lib.optionals (cfg.s3.enable && cfg.s3.secretAccessKeyFile != null) [
          "gradient_s3_secret_access_key:${cfg.s3.secretAccessKeyFile}"
        ] ++ lib.optionals cfg.githubApp.enable [
          "gradient_github_app_private_key:${cfg.githubApp.privateKeyFile}"
          "gradient_github_app_webhook_secret:${cfg.githubApp.webhookSecretFile}"
        ] ++ userPasswordFiles ++ orgPrivateKeyFiles ++ cacheSigningKeyFiles ++ apiKeyFiles
          ++ workerTokenFiles ++ projectCiTokenFiles;
      };

      environment = {
        NIX_REMOTE = "daemon";
        XDG_CACHE_HOME = "${cfg.baseDir}/www/.cache";
        GRADIENT_IP = cfg.listenAddr;
        GRADIENT_PORT = toString cfg.port;
        GRADIENT_SERVE_URL = "http${lib.optionalString cfg.useTls "s"}://${cfg.domain}";
        GRADIENT_FRONTEND_URL = cfg.frontend.url;
        GRADIENT_BASE_PATH = cfg.baseDir;
        GRADIENT_DATABASE_URL_FILE = "%d/gradient_database_url";
        GRADIENT_BINPATH_NIX = lib.getExe cfg.packages.nix;
        GRADIENT_BINPATH_SSH = lib.getExe' cfg.packages.ssh "ssh";
        GRADIENT_OIDC_ENABLED = lib.boolToString cfg.oidc.enable;
        GRADIENT_ENABLE_REGISTRATION = lib.boolToString cfg.settings.enableRegistration;
        GRADIENT_CRYPT_SECRET_FILE = "%d/gradient_crypt_secret";
        GRADIENT_JWT_SECRET_FILE = "%d/gradient_jwt_secret";
        GRADIENT_SERVE_CACHE = lib.boolToString cfg.serveCache;
        GRADIENT_REPORT_ERRORS = lib.boolToString cfg.reportErrors;
        GRADIENT_KEEP_EVALUATIONS = toString cfg.settings.keepEvaluations;
        GRADIENT_MAX_PROTO_CONNECTIONS = toString cfg.settings.maxProtoConnections;
        GRADIENT_LOG_LEVEL = cfg.settings.logLevel.default;
        GRADIENT_USE_TLS = lib.boolToString cfg.useTls;
        GRADIENT_QUIC = lib.boolToString cfg.enableQuic;
        GRADIENT_DISCOVERABLE = lib.boolToString cfg.discoverable;
        GRADIENT_FEDERATE_PROTO = lib.boolToString cfg.proto.federate;
        GRADIENT_DELETE_STATE = lib.boolToString cfg.settings.deleteState;
        GRADIENT_NAR_TTL_HOURS = toString cfg.settings.cacheTtlHours;
        GRADIENT_STATE_FILE = "%d/gradient_state";
        GRADIENT_CREDENTIALS_DIR = "%d";
        RUST_LOG = cfg.settings.logLevel.default;
      } // lib.optionalAttrs (cfg.settings.logLevel.cache != null) {
        GRADIENT_CACHE_LOG_LEVEL = cfg.settings.logLevel.cache;
      } // lib.optionalAttrs (cfg.settings.logLevel.web != null) {
        GRADIENT_WEB_LOG_LEVEL = cfg.settings.logLevel.web;
      } // lib.optionalAttrs (cfg.settings.logLevel.proto != null) {
        GRADIENT_PROTO_LOG_LEVEL = cfg.settings.logLevel.proto;
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
        GRADIENT_EMAIL_ENABLE_TLS = lib.boolToString cfg.email.enableTls;
      } // lib.optionalAttrs cfg.s3.enable {
        GRADIENT_S3_BUCKET = cfg.s3.bucket;
        GRADIENT_S3_REGION = cfg.s3.region;
        GRADIENT_S3_PREFIX = cfg.s3.prefix;
      } // lib.optionalAttrs (cfg.s3.enable && cfg.s3.endpoint != null) {
        GRADIENT_S3_ENDPOINT = cfg.s3.endpoint;
      } // lib.optionalAttrs (cfg.s3.enable && cfg.s3.accessKeyId != null) {
        GRADIENT_S3_ACCESS_KEY_ID = cfg.s3.accessKeyId;
      } // lib.optionalAttrs (cfg.s3.enable && cfg.s3.secretAccessKeyFile != null) {
        GRADIENT_S3_SECRET_ACCESS_KEY_FILE = "%d/gradient_s3_secret_access_key";
      } // lib.optionalAttrs cfg.githubApp.enable {
        GRADIENT_GITHUB_APP_ID = toString cfg.githubApp.appId;
        GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE = "%d/gradient_github_app_private_key";
        GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE = "%d/gradient_github_app_webhook_secret";
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
        virtualHosts."${cfg.domain}" = {
          enableACME = cfg.useTls;
          forceSSL = cfg.useTls;
          http2 = true;
          http3 = cfg.enableQuic;
          locations = {
            "/" = lib.mkIf cfg.frontend.enable {
              root = "${cfg.packages.frontend}/share/gradient-frontend";
              tryFiles = "$uri $uri/ /index.html";
            };

            "/api/" = {
              proxyPass = "http://127.0.0.1:${toString config.services.gradient.port}";
              proxyWebsockets = true;
            };

            "/proto" = lib.mkIf cfg.discoverable {
              proxyPass = "http://127.0.0.1:${toString config.services.gradient.port}";
              proxyWebsockets = true;
              extraConfig = ''
                proxy_connect_timeout 90d;
                proxy_send_timeout 90d;
                proxy_read_timeout 90d;
              '';
            };

            "/cache/" = lib.mkIf cfg.serveCache {
              proxyPass = "http://127.0.0.1:${toString config.services.gradient.port}";
              proxyWebsockets = true;
            };
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
