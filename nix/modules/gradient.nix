/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.services.gradient;
  logLevelType = lib.types.enum [ "trace" "debug" "info" "warn" "error" ];

  augmentedIntegrations = lib.mapAttrs (_: int: int // {
    has_secret_file = int.secret_file != null;
    has_access_token_file = int.access_token_file != null;
  }) cfg.state.integrations;

  stateJsonFile = pkgs.writers.writeJSON "gradient-state.json" (cfg.state // {
    integrations = augmentedIntegrations;
  });

  # GoBGP-style build-time check: run the server binary's `--validate-state`
  # over the generated state file so config errors fail the Nix build instead
  # of surfacing on first server start.
  validatedStateJsonFile = if cfg.validateState then
    pkgs.runCommand "gradient-state-validated.json" { } ''
      ${lib.getExe cfg.packages.server} --state-file ${stateJsonFile} --validate-state
      cp ${stateJsonFile} $out
    ''
  else
    stateJsonFile;

  userPasswordFiles = lib.concatLists (lib.mapAttrsToList (_: user:
    lib.optional (user.password_file != null)
      "gradient_user_${user.username}_password:${user.password_file}"
  ) cfg.state.users);
  orgPrivateKeyFiles = lib.mapAttrsToList (_: org: "gradient_org_${org.name}_private_key:${org.private_key_file}") cfg.state.organizations;
  cacheSigningKeyFiles = lib.mapAttrsToList (_: cache: "gradient_cache_${cache.name}_signing_key:${cache.signing_key_file}") cfg.state.caches;
  apiKeyFiles = lib.mapAttrsToList (_: api_key: "gradient_api_${api_key.name}_key:${api_key.key_file}") cfg.state.api_keys;
  workerTokenFiles = lib.mapAttrsToList (_: worker: "gradient_worker_${worker.worker_id}_token:${worker.token_file}") cfg.state.workers;
  integrationSecretFiles = lib.concatLists (lib.mapAttrsToList (_: int:
    lib.optional (int.secret_file != null)
      "gradient_integration_${int.name}_secret:${int.secret_file}"
  ) cfg.state.integrations);
  integrationTokenFiles = lib.concatLists (lib.mapAttrsToList (_: int:
    lib.optional (int.access_token_file != null)
      "gradient_integration_${int.name}_token:${int.access_token_file}"
  ) cfg.state.integrations);
  actionTokenFiles = lib.concatLists (lib.mapAttrsToList (_: project:
    lib.concatMap (action:
      let tokenFile = action.config.token_file or null; in
      lib.optional (action.type == "send_web_request" && tokenFile != null)
        "gradient_action_${action.name}_token:${tokenFile}"
    ) project.actions
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

      validateState = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Validate the generated `state` configuration at build time by running
          the server binary's `--validate-state` over it. Schema and
          cross-reference errors (unknown organizations, reporter triggers
          pointing at undeclared integrations, …) then fail the Nix build
          instead of the server on first start. No database is touched.
        '';
      };
      reverseProxy = {
        nginx.enable = lib.mkEnableOption "Nginx configuration" // {
          default = !cfg.reverseProxy.caddy.enable;
          defaultText = lib.literalExpression "!config.services.gradient.reverseProxy.caddy.enable";
        };

        caddy = {
          enable = lib.mkEnableOption "Caddy configuration";
          useACMEHost = lib.mkOption {
            description = ''
              A host of an existing Let’s Encrypt certificate to use.

              This options is directly passed to `services.caddy.virtualHosts.<name>.useACMEHost`
              and therefore does not create an ACME certificate.
            '';
            type = lib.types.nullOr lib.types.str;
            default = null;
          };

          extraConfig = lib.mkOption {
            description = ''
              Additional lines of configuration passed to
              `services.caddy.virtualHosts.<name>.extraConfig` 
              after the reverse proxy setup.
            '';
            type = lib.types.lines;
            default = "";
          };
        };
      };

      configurePostgres = lib.mkEnableOption "PostgreSQL configuration";
      reportErrors = lib.mkEnableOption "error reporting to Sentry";
      useTls = lib.mkEnableOption "TLS" // { default = true; };
      enableQuic = lib.mkEnableOption "Quic support";
      discoverable = lib.mkEnableOption "accept incoming connections on /proto" // { default = true; };
      packages = {
        server = lib.mkPackageOption pkgs "gradient" { };
        frontend = lib.mkPackageOption pkgs "gradient-frontend" { };
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

      metricsTokenFile = lib.mkOption {
        description = ''
          Path to a file containing the bearer token required to scrape
          `GET /metrics`. When null, the metrics endpoint is disabled
          (404).
        '';
        type = lib.types.nullOr lib.types.path;
        default = null;
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

      databaseMaxConnections = lib.mkOption {
        description = ''
          Maximum connections the scheduler / worker / cache pool may open.
          Total Postgres connections per gradient-server process is
          `databaseMaxConnections + databaseWebMaxConnections`. Raise only
          if Postgres' max_connections has headroom for it.
        '';
        type = lib.types.ints.positive;
        default = 32;
      };

      databaseMinConnections = lib.mkOption {
        description = "Minimum connections kept warm in the scheduler / worker / cache pool.";
        type = lib.types.ints.unsigned;
        default = 2;
      };

      databaseWebMaxConnections = lib.mkOption {
        description = "Maximum connections the axum HTTP pool may open.";
        type = lib.types.ints.positive;
        default = 16;
      };

      databaseWebMinConnections = lib.mkOption {
        description = "Minimum connections kept warm in the axum HTTP pool.";
        type = lib.types.ints.unsigned;
        default = 1;
      };

      proto = {
        public = lib.mkEnableOption "publicly accessible proto endpoint for federated builds and remote workers";
        federate = lib.mkEnableOption "federate Gradient Proto";
      };

      frontend = {
        enable = lib.mkEnableOption "Gradient Frontend" // { default = true; };
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

        virtualHostedStyle = lib.mkOption {
          description = ''
            Use virtual-hosted-style requests
            (`https://<bucket>.<endpoint>/key`) when a custom endpoint is
            configured. Defaults to `false`, which produces path-style URLs
            (`https://<endpoint>/<bucket>/key`) — required by MinIO,
            Garage, and most self-hosted S3-compatible backends. Set to
            `true` for providers that demand virtual-hosted addressing
            (Cloudflare R2 with a custom domain, certain Backblaze B2
            setups). Ignored when `endpoint` is null (AWS direct).
          '';
          type = lib.types.bool;
          default = false;
        };
      };

      settings = {
        enableRegistration = lib.mkEnableOption "registration. Users must be registered via OIDC." // { default = true; };
        sentryDsn = lib.mkOption {
          description = ''
            Override the Sentry DSN used when `reportErrors` is true.
            `null` (default) ships crash reports to the upstream Wavelens
            instance at `reports.wavelens.io`. Set this to your own Sentry
            DSN to keep reports in-house.
          '';
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "https://your-key@your-sentry.example.com/1";
        };
        maxProtoConnections = lib.mkOption {
          description = "Maximum number of simultaneous proto WebSocket connections";
          type = lib.types.ints.positive;
          default = 256;
        };

        keepEvaluations = lib.mkOption {
          description = "Amount of evaluations to keep in the database and cache";
          type = lib.types.ints.positive;
          default = 30;
        };

        logChunkBytes = lib.mkOption {
          description = "Target uncompressed size in bytes for each zstd build-log chunk written on finalize. Chunks split on line boundaries, so an over-long line may exceed this.";
          type = lib.types.ints.positive;
          default = 262144;
        };

        buildMaxAttempts = lib.mkOption {
          description = "Maximum number of build attempts before a transient failure becomes permanent (must be ≥ 1).";
          type = lib.types.ints.positive;
          default = 3;
        };

        buildRetryBackoffSecs = lib.mkOption {
          description = "Base backoff in seconds before retrying a transient build failure; doubled per prior attempt.";
          type = lib.types.ints.unsigned;
          default = 30;
        };

        buildDefaultTimeoutSecs = lib.mkOption {
          description = "Default wall-clock build timeout in seconds when the derivation sets no `timeout`. `0` disables.";
          type = lib.types.ints.unsigned;
          default = 3600;
        };

        buildDefaultMaxSilentSecs = lib.mkOption {
          description = "Default silent (no-output) build timeout in seconds when the derivation sets no `maxSilent`. `0` disables.";
          type = lib.types.ints.unsigned;
          default = 1800;
        };

        schedulerScoringPolicy = lib.mkOption {
          description = ''
            Scheduler scoring policy used to rank queued jobs against a
            requesting worker. `simple` weighs path availability, NAR size,
            dependency count, wait-time anti-starvation, builtin
            de-prioritization and fetch-worker reservation. `resource-aware`
            adds RAM/OOM-fit, CPU affinity, preferLocalBuild affinity and
            per-org fair-share, and is the default. Unknown values fall back to
            `resource-aware`.
          '';
          type = lib.types.enum [ "simple" "resource-aware" ];
          default = "resource-aware";
        };

        maxRequestSize = lib.mkOption {
          description = ''
            Maximum size in bytes of an HTTP request body for most endpoints.
            Caps webhook payloads, JSON bodies, etc. so an unbounded body
            cannot exhaust server memory. The build-request blob upload
            endpoint uses a fixed `MAX_BUILD_REQUEST_SIZE` (20 MiB) cap.
          '';
          type = lib.types.ints.positive;
          default = 2 * 1024 * 1024;
        };

        maxNarUploadSize = lib.mkOption {
          description = "Maximum size in bytes of a NAR upload to the cache upload endpoint.";
          type = lib.types.ints.positive;
          default = 512 * 1024 * 1024;
        };

        trustedProxies = lib.mkOption {
          description = ''
            CIDR allowlist of peers permitted to set `X-Forwarded-For`.
            Defaults to loopback so a reverse-proxy on the same host is
            trusted out of the box.
          '';
          type = lib.types.listOf lib.types.str;
          default = [ "127.0.0.1/32" "::1/128" ];
        };

        localIps = lib.mkOption {
          description = ''
            CIDR allowlist whose resolved client IPs receive a cache's
            `local_priority` (when set and non-zero). Defaults to the
            RFC1918 10/8 block.
          '';
          type = lib.types.listOf lib.types.str;
          default = [ "10.0.0.0/8" ];
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

        narStorageOpenTimeoutSecs = lib.mkOption {
          description = ''
            Maximum time the server will wait to open a NAR object stream
            from `nar_storage` (e.g. an S3 GET request) before giving up
            and emitting `NarUnavailable` to the worker. Caps how long a
            stalled storage backend can block a NarRequest before failing
            cleanly instead of hitting the worker's 600 s receive ceiling.
          '';
          type = lib.types.ints.positive;
          default = 60;
        };

        narSendChunkTimeoutSecs = lib.mkOption {
          description = ''
            Maximum time a single outbound `NarPush` chunk may sit in the
            per-connection writer queue waiting for the WebSocket sink to
            drain. Hitting this timeout indicates a stalled peer / TCP
            back-pressure and aborts the in-flight transfer with
            `NarAbort` rather than queuing unbounded data in memory.
          '';
          type = lib.types.ints.positive;
          default = 30;
        };

        maxConcurrentNarServes = lib.mkOption {
          description = ''
            Maximum number of NAR-serving tasks that may run concurrently
            per worker connection. Bounds memory and storage-backend
            fan-out when a worker requests many paths in a single batch.
          '';
          type = lib.types.ints.positive;
          default = 8;
        };

        maxNarBufferBytes = lib.mkOption {
          description = ''
            Maximum bytes a single proto session may hold in its inbound
            NAR upload buffers (open `NarPush` streams that haven't been
            finalised). Without this cap a rogue worker could open many
            streams without finalising them and pin unbounded RAM on the
            server (issue #109).
          '';
          type = lib.types.ints.positive;
          default = 10 * 1024 * 1024 * 1024;
        };

        allowAnonymousCache = lib.mkOption {
          description = ''
            Allow unauthenticated clients to access `GET /cache/{cache}/proto`
            for public caches. When false, anonymous handshakes are rejected
            with 403. Private caches always require an API key regardless.
          '';
          type = lib.types.bool;
          default = true;
        };

        anonMaxConnectionsPerIp = lib.mkOption {
          description = "Maximum simultaneous anonymous /cache/proto connections per client IP";
          type = lib.types.ints.positive;
          default = 32;
        };

        anonRatePerSecond = lib.mkOption {
          description = "Sustained request rate (per second) allowed for an anonymous proto session";
          type = lib.types.ints.positive;
          default = 20;
        };

        anonRateBurst = lib.mkOption {
          description = "Burst capacity for the anonymous proto session token bucket";
          type = lib.types.ints.positive;
          default = 200;
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
      {
        assertion = !(cfg.reverseProxy.nginx.enable && cfg.reverseProxy.caddy.enable);
        message = "You can only use one reverse proxy at a time";
      }
    ];

    systemd.services.gradient-server = {
      wantedBy = [ "multi-user.target" ];
      after = [
        "network.target"
        "systemd-tmpfiles-setup.service"
      ] ++ lib.optional cfg.configurePostgres "postgresql.target";

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
        ReadWritePaths = [ cfg.baseDir ];
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
          "gradient_state:${validatedStateJsonFile}"
        ] ++ lib.optional cfg.oidc.enable [
          "gradient_oidc_client_secret:${cfg.oidc.clientSecretFile}"
        ] ++ lib.optional cfg.email.enable [
          "gradient_email_smtp_password:${cfg.email.smtpPasswordFile}"
        ] ++ lib.optionals (cfg.s3.enable && cfg.s3.secretAccessKeyFile != null) [
          "gradient_s3_secret_access_key:${cfg.s3.secretAccessKeyFile}"
        ] ++ lib.optionals cfg.githubApp.enable [
          "gradient_github_app_private_key:${cfg.githubApp.privateKeyFile}"
          "gradient_github_app_webhook_secret:${cfg.githubApp.webhookSecretFile}"
        ] ++ lib.optional (cfg.metricsTokenFile != null)
          "gradient_metrics_token:${cfg.metricsTokenFile}"
        ++ userPasswordFiles ++ orgPrivateKeyFiles ++ cacheSigningKeyFiles ++ apiKeyFiles
          ++ workerTokenFiles ++ integrationSecretFiles ++ integrationTokenFiles
          ++ actionTokenFiles;
      };

      unitConfig = {
        StartLimitIntervalSec = 60;
        StartLimitBurst = 5;
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
        GRADIENT_DATABASE_MAX_CONNECTIONS = toString cfg.databaseMaxConnections;
        GRADIENT_DATABASE_MIN_CONNECTIONS = toString cfg.databaseMinConnections;
        GRADIENT_DATABASE_WEB_MAX_CONNECTIONS = toString cfg.databaseWebMaxConnections;
        GRADIENT_DATABASE_WEB_MIN_CONNECTIONS = toString cfg.databaseWebMinConnections;
        GRADIENT_OIDC_ENABLED = lib.boolToString cfg.oidc.enable;
        GRADIENT_ENABLE_REGISTRATION = lib.boolToString cfg.settings.enableRegistration;
        GRADIENT_CRYPT_SECRET_FILE = "%d/gradient_crypt_secret";
        GRADIENT_JWT_SECRET_FILE = "%d/gradient_jwt_secret";
        GRADIENT_REPORT_ERRORS = lib.boolToString cfg.reportErrors;
        GRADIENT_KEEP_EVALUATIONS = toString cfg.settings.keepEvaluations;
        GRADIENT_LOG_CHUNK_BYTES = toString cfg.settings.logChunkBytes;
        GRADIENT_BUILD_MAX_ATTEMPTS = toString cfg.settings.buildMaxAttempts;
        GRADIENT_BUILD_RETRY_BACKOFF_SECS = toString cfg.settings.buildRetryBackoffSecs;
        GRADIENT_BUILD_DEFAULT_TIMEOUT_SECS = toString cfg.settings.buildDefaultTimeoutSecs;
        GRADIENT_BUILD_DEFAULT_MAX_SILENT_SECS = toString cfg.settings.buildDefaultMaxSilentSecs;
        GRADIENT_SCHEDULER_SCORING_POLICY = cfg.settings.schedulerScoringPolicy;
        GRADIENT_MAX_REQUEST_SIZE = toString cfg.settings.maxRequestSize;
        GRADIENT_MAX_NAR_UPLOAD_SIZE = toString cfg.settings.maxNarUploadSize;
        GRADIENT_MAX_PROTO_CONNECTIONS = toString cfg.settings.maxProtoConnections;
        GRADIENT_LOG_LEVEL = cfg.settings.logLevel.default;
        GRADIENT_USE_TLS = lib.boolToString cfg.useTls;
        GRADIENT_QUIC = lib.boolToString cfg.enableQuic;
        GRADIENT_DISCOVERABLE = lib.boolToString cfg.discoverable;
        GRADIENT_FEDERATE_PROTO = lib.boolToString cfg.proto.federate;
        GRADIENT_DELETE_STATE = lib.boolToString cfg.settings.deleteState;
        GRADIENT_NAR_TTL_HOURS = toString cfg.settings.cacheTtlHours;
        GRADIENT_NAR_STORAGE_OPEN_TIMEOUT_SECS = toString cfg.settings.narStorageOpenTimeoutSecs;
        GRADIENT_NAR_SEND_CHUNK_TIMEOUT_SECS = toString cfg.settings.narSendChunkTimeoutSecs;
        GRADIENT_MAX_CONCURRENT_NAR_SERVES = toString cfg.settings.maxConcurrentNarServes;
        GRADIENT_MAX_NAR_BUFFER_BYTES = toString cfg.settings.maxNarBufferBytes;
        GRADIENT_PROTO_ALLOW_ANONYMOUS_CACHE = lib.boolToString cfg.settings.allowAnonymousCache;
        GRADIENT_PROTO_ANON_MAX_CONNECTIONS_PER_IP = toString cfg.settings.anonMaxConnectionsPerIp;
        GRADIENT_PROTO_ANON_RATE_PER_SECOND = toString cfg.settings.anonRatePerSecond;
        GRADIENT_PROTO_ANON_RATE_BURST = toString cfg.settings.anonRateBurst;
        GRADIENT_LOCAL_IPS = builtins.concatStringsSep "," cfg.settings.localIps;
        GRADIENT_TRUSTED_PROXIES = builtins.concatStringsSep "," cfg.settings.trustedProxies;
        GRADIENT_STATE_FILE = "%d/gradient_state";
        GRADIENT_CREDENTIALS_DIR = "%d";
        RUST_LOG = cfg.settings.logLevel.default;
      } // lib.optionalAttrs (cfg.settings.sentryDsn != null) {
        GRADIENT_SENTRY_DSN = cfg.settings.sentryDsn;
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
        GRADIENT_S3_VIRTUAL_HOSTED_STYLE = lib.boolToString cfg.s3.virtualHostedStyle;
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
      } // lib.optionalAttrs (cfg.metricsTokenFile != null) {
        GRADIENT_METRICS_TOKEN_FILE = "%d/gradient_metrics_token";
      };
    };

    services = {
      nginx = lib.mkIf cfg.reverseProxy.nginx.enable {
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
              proxyPass = "http://${config.services.gradient.listenAddr}:${toString config.services.gradient.port}";
              proxyWebsockets = true;
            };

            "/proto" = lib.mkIf (cfg.discoverable && cfg.proto.public) {
              proxyPass = "http://${config.services.gradient.listenAddr}:${toString config.services.gradient.port}";
              proxyWebsockets = true;
              extraConfig = ''
                proxy_connect_timeout 90d;
                proxy_send_timeout 90d;
                proxy_read_timeout 90d;
              '';
            };

            "/cache/" = {
              proxyPass = "http://${config.services.gradient.listenAddr}:${toString config.services.gradient.port}";
              proxyWebsockets = true;
            };
          };
        };
      };

      caddy = lib.mkIf cfg.reverseProxy.caddy.enable {
        enable = true;
        virtualHosts."${if cfg.useTls then "" else "http://"}${cfg.domain}" = {
          inherit (cfg.reverseProxy.caddy) useACMEHost;
          extraConfig = ''
            handle /api/* {
              reverse_proxy http://${cfg.listenAddr}:${toString cfg.port}
            }
            handle /cache/* {
              reverse_proxy http://${cfg.listenAddr}:${toString cfg.port}
            }
            handle /proto {
              reverse_proxy http://${cfg.listenAddr}:${toString cfg.port}
            }

            ${
              if cfg.frontend.enable then
                ''
                  handle {
                    root ${cfg.packages.frontend}/share/gradient-frontend
                    try_files {path} index.html
                    file_server
                  }
                ''
              else
                ""
            }

            ${cfg.reverseProxy.caddy.extraConfig}
          '';
        };
      };

      postgresql = lib.mkIf cfg.configurePostgres {
        enable = true;
        ensureDatabases = [ "gradient" ];
        settings.max_connections = lib.mkDefault 200;
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
