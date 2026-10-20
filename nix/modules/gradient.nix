/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.services.gradient;
  serverOptions = import ./options.nix {
    inherit lib;
    specs = builtins.fromJSON (builtins.readFile ../../backend/gradient-types/options.json);
    enabledGroups = [ "oidc" "scim" "email" "s3" "github_app" "ssh" ];
    withoutEnvironment = [ [ "credentialsDir" ] [ "database" "url" ] [ "state" "file" ] [ "state" "validate" ] [ "state" "delete" ] ];

    examples = {
      frontend.logoUrl = "https://example.com/logo.svg";
      log.traceDir = "/var/lib/gradient/trace";
      sentry.dsn = "https://your-key@your-sentry.example.com/1";
    };

    subGroups = {
      "" = [ "frontend" ];
      database = [ "cache" "web" ];
      email = [ "smtp" "from" ];
      eval = [ "cache" ];
      log = [ "level" ];
      metrics = [ "retention" "otlp" ];
      proto = [ "anonymous_cache" ];
    };

    writtenByHand = [
      [ "serveUrl" ]
      [ "credentialsDir" ]
      [ "frontend" "url" ]
      [ "database" "url" ]
      [ "nar" "chunkBytes" ]
      [ "email" "smtp" "host" ]
      [ "email" "smtp" "username" ]
      [ "email" "from" "address" ]
      [ "oidc" "clientId" ]
      [ "oidc" "discoveryUrl" ]
      [ "s3" "bucket" ]
      [ "githubApp" "id" ]
      [ "log" "level" "default" ]
      [ "log" "level" "web" ]
      [ "log" "level" "cache" ]
      [ "log" "level" "proto" ]
      [ "log" "level" "scheduler" ]
      [ "ssh" "enable" ]
      [ "state" "file" ]
      [ "state" "validate" ]
      [ "state" "delete" ]
    ];
  };

  logLevelType = lib.types.enum [ "trace" "debug" "info" "warn" "error" ];
  environmentValue = value: if lib.isBool value then lib.boolToString value else lib.mapNullable toString value;

  proxyMaxBodyBytes = lib.max cfg.http.maxRequestSize
    (lib.max cfg.nar.maxUploadSize cfg.http.maxSourceUploadSize);

  augmentedIntegrations = lib.mapAttrs (_: int: builtins.removeAttrs int [ "forge_type" ] // {
    has_secret_file = int.secret_file != null;
    has_access_token_file = int.access_token_file != null;
  }) cfg.state.integrations;

  stateJsonFile = pkgs.writers.writeJSON "gradient-state.json" (builtins.removeAttrs cfg.state [ "validate" "delete" ] // {
    integrations = augmentedIntegrations;
    caches = lib.mapAttrs (_: cache: builtins.removeAttrs cache [ "upstreams" ]) cfg.state.caches;
  });

  validatedStateJsonFile = if cfg.state.validate then
    pkgs.runCommand "gradient-state-validated.json" { __structuredAttrs = true; } ''
      ${lib.getExe cfg.packages.server} --state-file ${stateJsonFile} --state-validate
      cp ${stateJsonFile} $out
    ''
  else
    stateJsonFile;

  userPasswordFiles = lib.concatLists (lib.mapAttrsToList (_: user:
    lib.optional (user.password_file != null)
      "gradient_user_${user.username}_password:${user.password_file}"
  ) cfg.state.users);
  projectPrivateKeyFiles = lib.mapAttrsToList (_: project: "gradient_project_${project.name}_private_key:${project.private_key_file}") cfg.state.projects;
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
  localWorker = cfg.worker.enable && cfg.localWorker;

  identityHash = builtins.hashString "sha256" "gradient-local-worker:${config.networking.hostName}";
  localIdentity = lib.concatStringsSep "-" [
    (builtins.substring 0 8 identityHash)
    (builtins.substring 8 4 identityHash)
    (builtins.substring 12 4 identityHash)
    (builtins.substring 16 4 identityHash)
    (builtins.substring 20 12 identityHash)
  ];

  localTokenFile = "${cfg.worker.baseDir}/local-token";
  localPeersFile = "${cfg.worker.baseDir}/local-peers";

  actionSecretFields = {
    send_web_request = "token";
    send_matrix_message = "access_token";
    send_slack_message = "webhook_url";
  };

  actionSecretFiles = lib.concatLists (lib.mapAttrsToList (_: task:
    lib.concatMap (action:
      let
        field = actionSecretFields.${action.type} or null;
        file = if field == null then null else action.config."${field}_file" or null;
      in
      lib.optional (file != null) "gradient_action_${action.name}_${field}:${file}"
    ) task.actions
  ) cfg.state.tasks);
in {
  imports = [
    ./gradient-state.nix
    ./gradient-worker.nix
    ./renamed-options.nix
    { options.services.gradient = serverOptions.options; }
  ];

  options = {
    services.gradient = {
      enable = lib.mkEnableOption "Gradient, a Nix-CI for Teams";

      packages = {
        server = lib.mkPackageOption pkgs "gradient" { };
        frontend = lib.mkPackageOption pkgs "gradient-frontend" { };
      };

      environmentVariables = lib.mkOption {
        type = with lib.types; lazyAttrsOf (nullOr (oneOf [ bool int str path ]));
        default = { };
        example = { GRADIENT_GC_INTERVAL_SECS = 600; };
        description = ''
          Environment variables of the Gradient server.
          The [configuration reference](https://wavelens.github.io/gradient/reference/configuration) lists every variable and its default.
          Set a variable to `null` to unset it.
          Secrets belong into {option}`services.gradient.secrets`.
        '';
      };

      domain = lib.mkOption {
        type = lib.types.str;
        example = "gradient.example.com";
        description = "Domain under which Gradient is reachable.";
      };

      serveUrl = lib.mkOption {
        type = lib.types.str;
        default = "http${lib.optionalString cfg.useTls "s"}://${cfg.domain}";
        defaultText = lib.literalExpression ''"http''${lib.optionalString config.services.gradient.useTls "s"}://''${config.services.gradient.domain}"'';
        example = "http://localhost:8080";
        description = ''
          Public URL of Gradient. Set this for a URL other than
          {option}`services.gradient.domain`, for example behind a port mapping.
        '';
      };

      localWorker = lib.mkOption {
        type = lib.types.bool;
        default = cfg.worker.enable;
        defaultText = lib.literalExpression "config.services.gradient.worker.enable";
        description = ''
          Whether to register the {option}`services.gradient.worker` on this host automatically,
          with an ID from the hostname and a token and peers file from the first start.
          The state-declared team `server` is part of every new project.
        '';
      };

      reverseProxy = {
        nginx = {
          enable = lib.mkEnableOption "an nginx virtual host for Gradient" // {
            default = !cfg.reverseProxy.caddy.enable;
            defaultText = lib.literalExpression "!config.services.gradient.reverseProxy.caddy.enable";
          };

          manageTls = lib.mkOption {
            type = lib.types.bool;
            default = true;
            description = ''
              Whether to let nginx obtain and present the TLS certificate itself through the virtual
              host's `enableACME` and `forceSSL` settings. Disable it for upstream proxies (Traefik,
              Cloudflare, load balancers) that terminate TLS and forward plain HTTP to nginx. Keep
              {option}`services.gradient.useTls` enabled in that case for `https://` URLs and
              `Secure` session cookies. Has no effect with {option}`services.gradient.useTls`
              disabled.
            '';
          };
        };

        caddy = {
          enable = lib.mkEnableOption "a Caddy virtual host for Gradient";
          useACMEHost = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = ''
              Host of an existing ACME certificate, passed to
              {option}`services.caddy.virtualHosts.<name>.useACMEHost`. Caddy reuses it instead of
              requesting a new certificate. `null` keeps certificate management with Caddy.
            '';
          };

          extraConfig = lib.mkOption {
            type = lib.types.lines;
            default = "";
            description = ''
              Additional lines appended to {option}`services.caddy.virtualHosts.<name>.extraConfig`
              after the reverse proxy setup.
            '';
          };
        };
      };

      frontend = {
        enable = lib.mkEnableOption "the Gradient web frontend" // { default = true; };
        url = lib.mkOption {
          type = lib.types.str;
          default = cfg.serveUrl;
          defaultText = lib.literalExpression "config.services.gradient.serveUrl";
          example = "https://gradient.example.com";
          description = "Public URL of the Gradient frontend, used for links in CI status reports.";
        };

      };

      secrets = {
        jwtFile = lib.mkOption {
          type = lib.types.path;
          description = "File containing the secret used to sign JWTs.";
        };

        cryptFile = lib.mkOption {
          type = lib.types.path;
          description = "File containing the key used to encrypt secrets in the database.";
        };
      };

      postgres = {
        enable = lib.mkEnableOption "a local PostgreSQL database for Gradient";

        sharedBuffers = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = "512MB";
          example = "4GB";
          description = ''
            `shared_buffers` of the cluster set up by {option}`services.gradient.postgres.enable`.
            Size it to a quarter of the host's RAM. Gradient's working set is the build graph's
            indexes, and the stock 128 MB cannot keep them resident.

            This value is a single fixed allocation, unlike `work_mem` and `maintenance_work_mem`.
            The default is a quarter of the smallest supported host. Raise it on larger hosts. Set
            it to `null` to keep the PostgreSQL default.
          '';
        };

        effectiveCacheSize = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "12GB";
          description = ''
            `effective_cache_size` of the cluster set up by
            {option}`services.gradient.postgres.enable`. Set it to three quarters of the host's RAM.
            It is a planner hint, not an allocation. Set it to `null` to keep the PostgreSQL
            default.
          '';
        };

        workMem = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "32MB";
          description = ''
            `work_mem` of the cluster set up by {option}`services.gradient.postgres.enable`. Every
            sort or hash node in a query can claim this much. The scheduler's graph walks set a
            higher per-statement limit, except walks seeded by a root array, which keep the
            PostgreSQL default. Use `"32MB"` on a host sized for the three server pools; that is
            too much for a small host. `null` keeps the PostgreSQL default.
          '';
        };

        maintenanceWorkMem = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "1GB";
          description = ''
            `maintenance_work_mem` of the cluster set up by
            {option}`services.gradient.postgres.enable`, used for building indexes and by
            autovacuum. Each of `autovacuum_max_workers` can claim this much at once. Set it to
            `null` to keep the PostgreSQL default.
          '';
        };
      };

      database = {
        url = lib.mkOption {
          type = lib.types.str;
          default = "postgresql://gradient@localhost/gradient?host=/run/postgresql";
          description = "PostgreSQL connection URL.";
        };

        urlFile = lib.mkOption {
          type = lib.types.path;
          default = pkgs.writeText "database_url" cfg.database.url;
          defaultText = lib.literalExpression "pkgs.writeText \"database_url\" config.services.gradient.database.url;";
          example = "/etc/gradient/database_url";
          description = ''
            File containing the PostgreSQL connection URL, taking precedence over
            {option}`services.gradient.database.url`.
          '';
        };

      };

      proto = {
        public = lib.mkEnableOption "exposing `/proto` through the reverse proxy for remote workers and federation";
      };

      nar = {
        chunkBytes = lib.mkOption {
          type = lib.types.ints.between 4096 4194304;
          default = 524288;
          description = ''
            Size in bytes of each outbound `NarPush` chunk, from 4 KiB to 4 MiB.
            Smaller chunks can help slow or high-latency links like Tailscale over DERP.
          '';
        };

      };

      metrics = {
        tokenFile = lib.mkOption {
          type = lib.types.nullOr lib.types.path;
          default = null;
          description = ''
            File containing the bearer token required to scrape `GET /metrics`. Set it to `null` to
            disable the endpoint.
          '';
        };

      };

      log = {
        level = lib.mkOption {
          type = lib.types.submodule {
            options = {
              default = lib.mkOption {
                type = logLevelType;
                default = "info";
                description = "Default log level.";
              };

              cache = lib.mkOption {
                type = lib.types.nullOr logLevelType;
                default = null;
                description = ''
                  Log level of the cache. Set it to `null` to use
                  {option}`services.gradient.log.level.default`.
                '';
              };

              web = lib.mkOption {
                type = lib.types.nullOr logLevelType;
                default = null;
                description = ''
                  Log level of the web API. Set it to `null` to use
                  {option}`services.gradient.log.level.default`.
                '';
              };

              proto = lib.mkOption {
                type = lib.types.nullOr logLevelType;
                default = null;
                description = ''
                  Log level of the protocol layer. Set it to `null` to use
                  {option}`services.gradient.log.level.default`.
                '';
              };

              scheduler = lib.mkOption {
                type = lib.types.nullOr logLevelType;
                default = null;
                description = ''
                  Log level of the scheduler. Set it to `null` to use
                  {option}`services.gradient.log.level.default`.
                '';
              };
            };
          };
          default = { };
          description = "Log levels per component, overridden at runtime by {env}`RUST_LOG`.";
        };

      };

      oidc = {
        clientId = lib.mkOption {
          type = lib.types.str;
          description = "OIDC client ID.";
        };

        clientSecretFile = lib.mkOption {
          type = lib.types.path;
          description = "File containing the OIDC client secret.";
        };

        discoveryUrl = lib.mkOption {
          type = lib.types.str;
          description = "OIDC discovery URL.";
        };
      };

      scim = {
        tokenFile = lib.mkOption {
          type = lib.types.path;
          description = "File containing the SCIM bearer token.";
        };
      };

      email = {
        smtp = {
          host = lib.mkOption {
            type = lib.types.str;
            description = "SMTP server host name.";
          };

          username = lib.mkOption {
            type = lib.types.str;
            description = "SMTP user name.";
          };

          passwordFile = lib.mkOption {
            type = lib.types.path;
            description = "File containing the SMTP password.";
          };
        };

        from = {
          address = lib.mkOption {
            type = lib.types.str;
            description = "Sender email address.";
          };

        };
      };

      githubApp = {
        enable = lib.mkEnableOption "the GitHub App integration for webhooks and CI status reports";

        id = lib.mkOption {
          type = lib.types.ints.positive;
          description = "GitHub App ID, shown on the App's settings page.";
        };

        privateKeyFile = lib.mkOption {
          type = lib.types.path;
          description = "File containing the GitHub App's RS256 private key in PEM format.";
        };

        webhookSecretFile = lib.mkOption {
          type = lib.types.path;
          description = ''
            File containing the secret for verifying GitHub App webhook payloads. It must match the
            secret set on the App's webhook settings page.
          '';
        };
      };

      ssh = {
        enable = lib.mkEnableOption "the Nix daemon over SSH for `ssh-ng://` substituters, `nix copy` and `nixos-rebuild --build-host`";

        hostKeyFile = lib.mkOption {
          type = lib.types.nullOr lib.types.path;
          default = null;
          description = ''
            File containing the OpenSSH private host key. Set it to `null` to generate an ed25519
            key in {option}`services.gradient.baseDir` on first start.
          '';
        };

        openFirewall = lib.mkEnableOption "the SSH port in the firewall";
      };

      s3 = {
        enable = lib.mkEnableOption "storing NARs in S3";
        bucket = lib.mkOption {
          type = lib.types.str;
          default = "";
          description = ''
            Name of the S3 bucket. The bucket must not have versioning, object lock or replication
            enabled. Object overwrites happen in place, and old versions stay in the bucket.
            Versioned buckets thus keep an unreclaimable copy per upload.
          '';
        };

        secretAccessKeyFile = lib.mkOption {
          type = lib.types.nullOr lib.types.path;
          default = null;
          description = ''
            File containing the AWS secret access key. Set it to `null` to use instance credentials.
          '';
        };

      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [{
      assertion = cfg.localWorker -> cfg.worker.enable;
      message = "services.gradient.localWorker provisions credentials for a worker on this host and requires services.gradient.worker.enable.";
    } {
      assertion = cfg.proto.federate -> cfg.proto.discoverable;
      message = "proto.federate requires proto.discoverable to be enabled";
    } {
      assertion = !(cfg.reverseProxy.nginx.enable && cfg.reverseProxy.caddy.enable);
      message = "You can only use one reverse proxy at a time";
    }];

    networking.firewall.allowedTCPPorts = lib.mkIf (cfg.ssh.enable && cfg.ssh.openFirewall) [ cfg.ssh.port ];
    systemd = {
      tmpfiles.settings."10-gradient-trace" = lib.mkIf (cfg.log.traceDir != null) {
        ${cfg.log.traceDir}.d = {
          user = "gradient";
          group = "gradient";
          mode = "0750";
        };
      };

      services = {
        gradient-local-worker-token = lib.mkIf localWorker {
          description = "Gradient local worker credentials";
          requiredBy = [ "gradient-worker.service" ];
          before = [ "gradient-worker.service" ];

          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            User = "gradient-worker";
            Group = "gradient-worker";
            StateDirectory = "gradient-worker";
            UMask = "0277";
          };

          script = ''
            rm -f ${localTokenFile}.new ${localPeersFile}.new
            if [ ! -s ${localTokenFile} ]; then
              ${lib.getExe pkgs.openssl} rand -base64 48 > ${localTokenFile}.new
              mv ${localTokenFile}.new ${localTokenFile}
            fi
            printf '*:%s\n' "$(cat ${localTokenFile})" > ${localPeersFile}.new
            mv ${localPeersFile}.new ${localPeersFile}
          '';
        };

        gradient-worker = lib.mkIf localWorker {
          after = [ "gradient-local-worker-token.service" ];
        };

        gradient-server = {
          wantedBy = [ "multi-user.target" ];
          after = [
            "network.target"
            "systemd-tmpfiles-setup.service"
          ] ++ lib.optional cfg.postgres.enable "postgresql.target"
            ++ lib.optional localWorker "gradient-local-worker-token.service";
          requires = lib.optional localWorker "gradient-local-worker-token.service";

          serviceConfig = {
            Type = "notify";
            ExecStart = lib.getExe cfg.packages.server;
            TimeoutStartSec = "infinity";
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
            ReadWritePaths = [ cfg.baseDir ] ++ lib.optional (cfg.log.traceDir != null) cfg.log.traceDir;
            Restart = "on-failure";
            RestartSec = 10;
            LimitNOFILE = 65535;
            RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
            RestrictNamespaces = true;
            RestrictRealtime = true;
            RestrictSUIDSGID = true;
            WorkingDirectory = cfg.baseDir;
            LoadCredential = [
              "gradient_database_url:${cfg.database.urlFile}"
              "gradient_crypt_secret:${cfg.secrets.cryptFile}"
              "gradient_jwt_secret:${cfg.secrets.jwtFile}"
              "gradient_state:${validatedStateJsonFile}"
            ] ++ lib.optionals cfg.oidc.enable [
              "gradient_oidc_client_secret:${cfg.oidc.clientSecretFile}"
            ] ++ lib.optionals cfg.scim.enable [
              "gradient_scim_token:${cfg.scim.tokenFile}"
            ] ++ lib.optionals cfg.email.enable [
              "gradient_email_smtp_password:${cfg.email.smtp.passwordFile}"
            ] ++ lib.optionals (cfg.s3.enable && cfg.s3.secretAccessKeyFile != null) [
              "gradient_s3_secret_access_key:${cfg.s3.secretAccessKeyFile}"
            ] ++ lib.optionals cfg.githubApp.enable [
              "gradient_github_app_private_key:${cfg.githubApp.privateKeyFile}"
              "gradient_github_app_webhook_secret:${cfg.githubApp.webhookSecretFile}"
            ] ++ lib.optional (cfg.ssh.enable && cfg.ssh.hostKeyFile != null)
              "gradient_ssh_host_key:${cfg.ssh.hostKeyFile}"
            ++ lib.optional (cfg.metrics.tokenFile != null)
              "gradient_metrics_token:${cfg.metrics.tokenFile}"
            ++ userPasswordFiles ++ projectPrivateKeyFiles ++ cacheSigningKeyFiles ++ apiKeyFiles
              ++ workerTokenFiles ++ integrationSecretFiles ++ integrationTokenFiles
              ++ actionSecretFiles;
          };

          unitConfig = {
            StartLimitIntervalSec = 60;
            StartLimitBurst = 5;
          };

          environment = {
            NIX_REMOTE = "daemon";
            XDG_CACHE_HOME = "${cfg.baseDir}/www/.cache";
            GRADIENT_SECRETS_CRYPT_FILE = "%d/gradient_crypt_secret";
            GRADIENT_SECRETS_JWT_FILE = "%d/gradient_jwt_secret";
            GRADIENT_STATE_FILE = "%d/gradient_state";
            GRADIENT_STATE_DELETE = lib.boolToString cfg.state.delete;
            GRADIENT_CREDENTIALS_DIR = "%d";
            GRADIENT_DATABASE_URL_FILE = "%d/gradient_database_url";
          } // lib.optionalAttrs cfg.oidc.enable {
            GRADIENT_OIDC_CLIENT_SECRET_FILE = "%d/gradient_oidc_client_secret";
          } // lib.optionalAttrs cfg.scim.enable {
            GRADIENT_SCIM_TOKEN_FILE = "%d/gradient_scim_token";
          } // lib.optionalAttrs cfg.email.enable {
            GRADIENT_EMAIL_SMTP_PASSWORD_FILE = "%d/gradient_email_smtp_password";
          } // lib.optionalAttrs (cfg.s3.enable && cfg.s3.secretAccessKeyFile != null) {
            GRADIENT_S3_SECRET_ACCESS_KEY_FILE = "%d/gradient_s3_secret_access_key";
          } // lib.optionalAttrs cfg.githubApp.enable {
            GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE = "%d/gradient_github_app_private_key";
            GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE = "%d/gradient_github_app_webhook_secret";
          } // lib.optionalAttrs (cfg.ssh.enable && cfg.ssh.hostKeyFile != null) {
            GRADIENT_SSH_HOST_KEY_FILE = "%d/gradient_ssh_host_key";
          } // lib.optionalAttrs (cfg.metrics.tokenFile != null) {
            GRADIENT_METRICS_TOKEN_FILE = "%d/gradient_metrics_token";
          } // serverOptions.environment cfg // lib.mapAttrs (_: environmentValue) cfg.environmentVariables;
        };
      };
    };

    services = {
      gradient = lib.mkIf localWorker {
        state.teams.server = {
          display_name = "Server";
          new_projects.workers = true;
        };

        state.workers.local = {
          display_name = "Local Worker";
          worker_id = localIdentity;
          token_file = localTokenFile;
          team = "server";
        };

        worker = {
          id = lib.mkDefault localIdentity;
          peersFile = lib.mkDefault localPeersFile;
          serverUrl = lib.mkDefault "ws://127.0.0.1:${toString cfg.port}/proto";
        };
      };

      nginx = lib.mkIf cfg.reverseProxy.nginx.enable {
        enable = true;
        virtualHosts."${cfg.domain}" = {
          enableACME = lib.mkDefault (cfg.useTls && cfg.reverseProxy.nginx.manageTls);
          forceSSL = lib.mkDefault (cfg.useTls && cfg.reverseProxy.nginx.manageTls);
          http2 = true;
          http3 = lib.mkDefault cfg.useQuic;
          locations = {
            "/" = lib.mkIf cfg.frontend.enable {
              root = "${cfg.packages.frontend}/share/gradient-frontend";
              tryFiles = "$uri $uri/ /index.html";
            };

            "/api/" = {
              proxyPass = "http://${config.services.gradient.listenAddr}:${toString config.services.gradient.port}";
              proxyWebsockets = true;
              extraConfig = ''
                client_max_body_size ${toString proxyMaxBodyBytes};
                proxy_buffering off;
                proxy_request_buffering off;
                proxy_connect_timeout 1h;
                proxy_send_timeout 1h;
                proxy_read_timeout 1h;
              '';
            };

            "/proto" = lib.mkIf (cfg.proto.discoverable && cfg.proto.public) {
              proxyPass = "http://${config.services.gradient.listenAddr}:${toString config.services.gradient.port}";
              proxyWebsockets = true;
              extraConfig = ''
                proxy_buffer_size 256k;
                proxy_buffers 4 256k;
                proxy_connect_timeout 90d;
                proxy_send_timeout 90d;
                proxy_read_timeout 90d;
              '';
            };

            "~ ^/cache/[^/]+/proto$" = {
              proxyPass = "http://${config.services.gradient.listenAddr}:${toString config.services.gradient.port}";
              proxyWebsockets = true;
              extraConfig = ''
                proxy_buffer_size 256k;
                proxy_buffers 4 256k;
                proxy_connect_timeout 1d;
                proxy_send_timeout 1d;
                proxy_read_timeout 1d;
              '';
            };

            "/cache/" = {
              proxyPass = "http://${config.services.gradient.listenAddr}:${toString config.services.gradient.port}";
              proxyWebsockets = true;
              extraConfig = ''
                client_max_body_size ${toString proxyMaxBodyBytes};
                proxy_buffering off;
                proxy_request_buffering off;
                proxy_connect_timeout 1h;
                proxy_send_timeout 1h;
                proxy_read_timeout 1h;
              '';
            };
          };
        };
      };

      caddy = lib.mkIf cfg.reverseProxy.caddy.enable {
        enable = true;
        virtualHosts."${if cfg.useTls then "" else "http://"}${cfg.domain}" = {
          inherit (cfg.reverseProxy.caddy) useACMEHost;
          extraConfig = ''
            request_body {
              max_size ${toString proxyMaxBodyBytes}
            }
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

      postgresql = lib.mkIf cfg.postgres.enable {
        enable = true;
        ensureDatabases = [ "gradient" ];
        settings = {
          max_connections = lib.mkDefault 200;
          random_page_cost = lib.mkDefault 1.1;
          max_locks_per_transaction = lib.mkDefault 1024;
          wal_compression = lib.mkDefault "lz4";
          wal_buffers = lib.mkDefault "64MB";
          checkpoint_timeout = lib.mkDefault "15min";
        } // lib.optionalAttrs (cfg.postgres.sharedBuffers != null) {
          shared_buffers = cfg.postgres.sharedBuffers;
        } // lib.optionalAttrs (cfg.postgres.effectiveCacheSize != null) {
          effective_cache_size = cfg.postgres.effectiveCacheSize;
        } // lib.optionalAttrs (cfg.postgres.workMem != null) {
          work_mem = cfg.postgres.workMem;
        } // lib.optionalAttrs (cfg.postgres.maintenanceWorkMem != null) {
          maintenance_work_mem = cfg.postgres.maintenanceWorkMem;
        };

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
