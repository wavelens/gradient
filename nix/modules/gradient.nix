/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, ... }: let
  cfg = config.services.gradient;
  logLevelType = lib.types.enum [ "trace" "debug" "info" "warn" "error" ];

  # Reverse-proxy body cap tracks the largest configured backend upload limit
  # so the proxy is never the bottleneck for source/NAR uploads.
  proxyMaxBodyBytes = lib.max cfg.http.maxRequestSize
    (lib.max cfg.nar.maxUploadSize cfg.http.maxSourceUploadSize);

  augmentedIntegrations = lib.mapAttrs (_: int: int // {
    has_secret_file = int.secret_file != null;
    has_access_token_file = int.access_token_file != null;
  }) cfg.state.integrations;

  stateJsonFile = pkgs.writers.writeJSON "gradient-state.json" (builtins.removeAttrs cfg.state [ "validate" "delete" ] // {
    integrations = augmentedIntegrations;
  });

  # GoBGP-style build-time check: run the server binary's `--state-validate`
  # over the generated state file so config errors fail the Nix build instead
  # of surfacing on first server start.
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
  # A worker on this host provisions its own registration, so the admin never
  # sees a token or a UUID. Its credential file is generated before either
  # service starts; the server only reads it through LoadCredential. All of it
  # lives here rather than in the worker module, which is also deployed on its
  # own and must not reach for the server's options.
  localWorker = cfg.worker.enable && cfg.localWorker;

  # Derived from the hostname so it is stable across rebuilds and known at eval
  # time, which is what lets the server pre-register the worker before either
  # service has ever run.
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

  actionTokenFiles = lib.concatLists (lib.mapAttrsToList (_: task:
    lib.concatMap (action:
      let tokenFile = action.config.token_file or null; in
      lib.optional (action.type == "send_web_request" && tokenFile != null)
        "gradient_action_${action.name}_token:${tokenFile}"
    ) task.actions
  ) cfg.state.tasks);
in {
  # disabledModules = [
  #   "services/gradient/default.nix"
  #   "services/gradient/worker.nix"
  #   "services/gradient/state.nix"
  # ];

  imports = [
    ./gradient-state.nix
    ./gradient-worker.nix
    (lib.mkRemovedOptionModule [ "services" "gradient" "nar" "uploadConcurrency" ] "replaced by services.gradient.upload.concurrency and services.gradient.upload.bytesBudget")
    (lib.mkRemovedOptionModule [ "services" "gradient" "nar" "commitConcurrency" ] "replaced by services.gradient.upload.concurrency and services.gradient.upload.bytesBudget")
    (lib.mkRemovedOptionModule [ "services" "gradient" "nar" "maxBufferBytes" ] "replaced by services.gradient.upload.concurrency and services.gradient.upload.bytesBudget")
    (lib.mkRemovedOptionModule [ "services" "gradient" "scheduler" "recordCandidates" ] "runner-up candidates were never recorded")
  ];

  options = {
    services.gradient = {
      enable = lib.mkEnableOption "Gradient";

      packages = {
        server = lib.mkPackageOption pkgs "gradient" { };
        frontend = lib.mkPackageOption pkgs "gradient-frontend" { };
      };

      domain = lib.mkOption {
        type = lib.types.str;
        example = "gradient.example.com";
        description = "Domain under which Gradient is served.";
      };

      listenAddr = lib.mkOption {
        type = lib.types.str;
        default = "127.0.0.1";
        description = "IP address the Gradient server listens on.";
      };

      port = lib.mkOption {
        type = lib.types.port;
        default = 3000;
        description = "Port the Gradient server listens on.";
      };

      baseDir = lib.mkOption {
        type = lib.types.path;
        default = "/var/lib/gradient";
        description = "Directory holding Gradient's state, NAR files and caches.";
      };

      useTls = lib.mkEnableOption "TLS" // { default = true; };

      useQuic = lib.mkEnableOption "advertising HTTP/3 (QUIC) to clients";

      localWorker = lib.mkOption {
        type = lib.types.bool;
        default = cfg.worker.enable;
        defaultText = lib.literalExpression "config.services.gradient.worker.enable";
        description = ''
          Whether to provision credentials for a {option}`services.gradient.worker` running on this
          host: a worker identity derived from the hostname, a token generated on first start, the
          matching peers file, and a registration as an `auto_enable` base worker. No UUID, token or
          web UI registration step is needed.

          Disable to run a co-located worker that authenticates like a remote one, with
          {option}`services.gradient.worker.id` and {option}`services.gradient.worker.peersFile` set
          by hand.
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
              Whether nginx obtains and serves the TLS certificate itself, by setting the virtual
              host's `enableACME` and `forceSSL`. Disable when TLS is terminated by an upstream
              proxy (Traefik, Cloudflare, a load balancer) that forwards plain HTTP to nginx, and
              keep {option}`services.gradient.useTls` enabled so Gradient still emits `https://`
              URLs and marks session cookies `Secure`. Has no effect when
              {option}`services.gradient.useTls` is disabled.
            '';
          };
        };

        caddy = {
          enable = lib.mkEnableOption "a Caddy virtual host for Gradient";
          useACMEHost = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = ''
              Host of an existing ACME certificate to use, passed to
              {option}`services.caddy.virtualHosts.<name>.useACMEHost`. No certificate is requested
              for it.
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
          default = "http${lib.optionalString cfg.useTls "s"}://${cfg.domain}";
          defaultText = lib.literalExpression ''http''${lib.optionalString config.services.gradient.useTls "s"}://''${config.services.gradient.domain}'';
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
            Size it to a quarter of the host's RAM: Gradient's working set is the build graph's
            indexes, which the stock 128 MB cannot keep resident.

            Unlike `work_mem` and `maintenance_work_mem` this is one fixed allocation, so the
            default of a quarter of the smallest supported host is a floor to raise on larger hosts.
            `null` keeps the PostgreSQL default.
          '';
        };

        effectiveCacheSize = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "12GB";
          description = ''
            `effective_cache_size` of the cluster set up by
            {option}`services.gradient.postgres.enable`, typically three quarters of the host's RAM.
            It is a planner hint, not an allocation. `null` keeps the PostgreSQL default.
          '';
        };

        workMem = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "32MB";
          description = ''
            `work_mem` of the cluster set up by {option}`services.gradient.postgres.enable`. It is
            the floor every query gets; graph walks raise their own ceiling for a single statement.
            It is charged per sort or hash node, so `"32MB"` suits a host sized for the three server
            pools and is too much for a small one. `null` keeps the PostgreSQL default.
          '';
        };

        maintenanceWorkMem = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "1GB";
          description = ''
            `maintenance_work_mem` of the cluster set up by
            {option}`services.gradient.postgres.enable`, used by index builds and autovacuum. Each
            of `autovacuum_max_workers` can claim this much at once. `null` keeps the PostgreSQL
            default.
          '';
        };
      };

      database = {
        url = lib.mkOption {
          type = lib.types.str;
          # Peer auth on the local socket demands the role match the unit's system
          # user, and sqlx no longer infers one from the process: with no user in
          # the URL it asks whoami, which answers "anonymous" under systemd.
          default = "postgresql://gradient@localhost/gradient?host=/run/postgresql";
          description = "PostgreSQL connection URL.";
        };

        urlFile = lib.mkOption {
          type = lib.types.path;
          default = pkgs.writeText "database_url" cfg.database.url;
          defaultText = lib.literalExpression "pkgs.writeText \"database_url\" config.services.gradient.database.url;";
          example = "/etc/gradient/database_url";
          description = ''
            File containing the PostgreSQL connection URL. Takes precedence over
            {option}`services.gradient.database.url`.
          '';
        };

        maxConnections = lib.mkOption {
          type = lib.types.ints.positive;
          default = 16;
          description = ''
            Maximum connections of the scheduler and worker pool. Each server process opens up to
            the sum of {option}`services.gradient.database.maxConnections`,
            {option}`services.gradient.database.cache.maxConnections` and
            {option}`services.gradient.database.web.maxConnections`, which PostgreSQL's
            `max_connections` must accommodate.
          '';
        };

        minConnections = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 2;
          description = "Minimum connections kept open in the scheduler and worker pool.";
        };

        cache = {
          maxConnections = lib.mkOption {
            type = lib.types.ints.positive;
            default = 32;
            description = ''
              Maximum connections of the cache query pool. It is separate from the scheduler pool so
              a large evaluation's prefetch traffic cannot starve dispatch.
            '';
          };

          minConnections = lib.mkOption {
            type = lib.types.ints.unsigned;
            default = 2;
            description = "Minimum connections kept open in the cache query pool.";
          };
        };

        web = {
          maxConnections = lib.mkOption {
            type = lib.types.ints.positive;
            default = 8;
            description = "Maximum connections of the HTTP API pool.";
          };

          minConnections = lib.mkOption {
            type = lib.types.ints.unsigned;
            default = 1;
            description = "Minimum connections kept open in the HTTP API pool.";
          };
        };
      };

      registration = {
        enable = lib.mkEnableOption "self-service user registration" // { default = true; };
      };

      sentry = {
        enable = lib.mkEnableOption "error reporting to Sentry";

        dsn = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "https://your-key@your-sentry.example.com/1";
          description = ''
            Sentry DSN used when {option}`services.gradient.sentry.enable` is set. `null` sends
            reports to the upstream Wavelens instance at `reports.wavelens.io`.
          '';
        };
      };

      pullRequests = {
        commitName = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = ''
            Git author and committer name for commits pushed by the `open_pr` action. `null` lets
            each forge choose: GitHub credits the App bot and marks the commit verified, Gitea,
            Forgejo and GitLab use the token owner (which needs the `read:user` or `read_user`
            scope) and fall back to `Gradient <gradient@users.noreply.HOST>`.
          '';
        };

        commitEmail = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = ''
            Git author and committer email for commits pushed by the `open_pr` action. See
            {option}`services.gradient.pullRequests.commitName`.
          '';
        };
      };

      permissions = {
        createProject = lib.mkOption {
          type = lib.types.enum [ "none" "superusers" "everyone" ];
          default = "everyone";
          description = ''
            Who may create projects through the API: `none` (only the declarative state),
            `superusers`, or `everyone` (any authenticated user).
          '';
        };

        createCache = lib.mkOption {
          type = lib.types.enum [ "none" "superusers" "everyone" ];
          default = "everyone";
          description = ''
            Who may create caches through the API: `none` (only the declarative state),
            `superusers`, or `everyone` (any authenticated user).
          '';
        };
      };

      http = {
        maxRequestSize = lib.mkOption {
          type = lib.types.ints.positive;
          default = 2 * 1024 * 1024;
          description = ''
            Maximum HTTP request body size in bytes for most endpoints, so an unbounded body cannot
            exhaust server memory. Build request blob uploads use a fixed 20 MiB cap.
          '';
        };

        maxSourceUploadSize = lib.mkOption {
          type = lib.types.ints.positive;
          default = 512 * 1024 * 1024;
          description = ''
            Maximum size in bytes of a source upload to `POST /build-requests/source` (as sent by
            `gradient build`) and of a chunked manifest in total. The built-in reverse proxy's body
            size limit is raised to fit it.
          '';
        };

        trustedProxies = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = [ "127.0.0.1/8" "::1/128" ];
          description = ''
            CIDR ranges of peers allowed to set `X-Forwarded-For`. The default trusts a reverse
            proxy on the same host.
          '';
        };

        localIps = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = [ "192.168.0.0/16" "172.16.0.0/12" "100.64.0.0/10" "10.0.0.0/8" "fc00::/7" ];
          description = ''
            CIDR ranges whose clients receive a cache's `local_priority`, when that is set and
            non-zero.
          '';
        };
      };

      proto = {
        public = lib.mkEnableOption "exposing `/proto` through the reverse proxy for remote workers and federation";
        federate = lib.mkEnableOption "federation with other Gradient servers over `/proto`";
        discoverable = lib.mkEnableOption "incoming worker and federation connections on `/proto`" // { default = true; };

        maxConnections = lib.mkOption {
          type = lib.types.ints.positive;
          default = 256;
          description = "Maximum simultaneous `/proto` WebSocket connections.";
        };

        workerHeartbeatTimeoutSecs = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 120;
          description = ''
            Seconds a connected worker may stay silent before the server declares it dead and
            re-queues its jobs. Workers heartbeat every 10 seconds, so the default tolerates twelve
            missed beats. This is the only detection for a worker lost without a clean TCP close
            (OOM kill, frozen host, network partition). `0` disables the watchdog.
          '';
        };

        anonymousCache = {
          enable = lib.mkOption {
            type = lib.types.bool;
            default = true;
            description = ''
              Whether unauthenticated clients may use `GET /cache/{cache}/proto` for public caches.
              Private caches always require an API key.
            '';
          };

          maxConnectionsPerIp = lib.mkOption {
            type = lib.types.ints.positive;
            default = 32;
            description = ''
              Maximum simultaneous anonymous `/cache/proto` connections per client IP.
            '';
          };
        };
      };

      upload = {
        concurrency = lib.mkOption {
          type = lib.types.ints.positive;
          default = 16;
          description = ''
            Uploads (NARs and eval cache blobs) admitted at once across all workers and REST
            clients. Further uploads wait for a permit.
          '';
        };

        bytesBudget = lib.mkOption {
          type = lib.types.ints.positive;
          default = 8589934592;
          description = ''
            Total size in bytes of admitted uploads. An upload that does not fit waits; one larger
            than the budget runs alone once nothing else is in flight.
          '';
        };

        leaseIdleSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 300;
          description = ''
            Seconds a granted relay upload may go without data before its permit is reclaimed and
            the worker is told to retry.
          '';
        };

        restWaitSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 30;
          description = ''
            Seconds a NAR upload to the cache upload endpoint waits for a permit before it is
            answered with 503 and `Retry-After`.
          '';
        };
      };

      nar = {
        maxUploadSize = lib.mkOption {
          type = lib.types.ints.positive;
          default = 512 * 1024 * 1024;
          description = "Maximum size in bytes of a NAR uploaded to the cache upload endpoint.";
        };

        smallBytes = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 1024 * 1024;
          description = ''
            Size in bytes up to which a NAR is served through the server on download instead of a
            presigned S3 URL, and kept in the in-memory cache. Uploads do not depend on it.
          '';
        };

        hotCacheBytes = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 512 * 1024 * 1024;
          description = "Capacity in bytes of the in-memory NAR cache. `0` disables it.";
        };

        verifyDigest = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = ''
            Whether to download NARs committed through presigned S3 uploads and verify their hash,
            catching same-length corruption at the cost of a full object read. Without it the size
            is still checked; relayed and REST uploads are always verified.
          '';
        };

        storageOpenTimeoutSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 60;
          description = ''
            Seconds to wait for a NAR object stream from storage (for example an S3 GET) before
            answering the worker with `NarUnavailable`.
          '';
        };

        sendChunkTimeoutSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 30;
          description = ''
            Seconds an outbound `NarPush` chunk may wait for the WebSocket to drain before the
            transfer is aborted with `NarAbort`.
          '';
        };

        maxConcurrentServes = lib.mkOption {
          type = lib.types.ints.positive;
          default = 8;
          description = ''
            NAR serving tasks that may run at once per worker connection, bounding memory and
            storage fan-out for large batches.
          '';
        };

        partialTtlSecs = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 86400;
          description = ''
            Seconds after its last write that an unfinished NAR upload under
            {file}`<services.gradient.baseDir>/nar-partial` is deleted. `0` disables the cleanup.
          '';
        };
      };

      cache = {
        upstreamQueryConcurrency = lib.mkOption {
          type = lib.types.ints.positive;
          default = 32;
          description = ''
            Maximum simultaneous narinfo requests to upstream caches across the server.
          '';
        };

        maxStorageGb = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 0;
          description = ''
            Instance-wide limit on cached NAR storage in GB. When every writable cache of a project
            has less than 10 MiB left, new evaluations wait. `0` disables the limit; per-cache
            limits still apply.
          '';
        };

        signSweepIntervalSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 3600;
          description = ''
            Seconds between NAR signature backfill runs. Uploads are signed immediately, so this
            only catches subscription placeholders and unsigned leftovers.
          '';
        };

        debugIndexIntervalSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 300;
          description = ''
            Seconds between DWARF build ID index backfill runs. Uploads are indexed immediately, so
            this only catches paths cached before the index existed or interrupted by a restart.
          '';
        };
      };

      gc = {
        intervalSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 3600;
          description = "Seconds between garbage collection runs.";
        };

        narTtlHours = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 336;
          description = ''
            Hours a cached path outside the live closure of retained evaluations is kept after its
            last fetch, or its upload if never fetched.
            {option}`services.gradient.gc.narUploadGraceHours` always applies on top.
          '';
        };

        narUploadGraceHours = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 24;
          description = ''
            Hours before an unreferenced NAR object is deleted, covering the window between its
            upload and the commit of its database rows.
          '';
        };

        orphanDerivationHours = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 24;
          description = ''
            Hours before a derivation outside the build closure of every retained evaluation is
            deleted. The grace lets a quick re-evaluation reuse it. `0` deletes it on the next run.
          '';
        };

        wedgedEvalHours = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 24;
          description = ''
            Hours an evaluation may stay in one phase before it is considered stuck and stops
            blocking evaluation garbage collection. `0` blocks forever.
          '';
        };
      };

      eval = {
        maxKeep = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 30;
          description = ''
            Maximum number of evaluations kept per task. It caps the per-task setting, and new tasks
            start at the lower of 30 and this value. `0` disables the limit.
          '';
        };

        cache = {
          maxTotalBytes = lib.mkOption {
            type = lib.types.ints.unsigned;
            default = 10 * 1024 * 1024 * 1024;
            description = ''
              Total size in bytes of shared eval cache blobs. Older blobs are evicted until the
              total fits.
            '';
          };

          maxAgeDays = lib.mkOption {
            type = lib.types.ints.unsigned;
            default = 30;
            description = ''
              Days after which an eval cache blob is evicted regardless of the size limit.
            '';
          };

          sweepIntervalSecs = lib.mkOption {
            type = lib.types.ints.positive;
            default = 3600;
            description = "Seconds between eval cache eviction runs.";
          };
        };
      };

      build = {
        maxAttempts = lib.mkOption {
          type = lib.types.ints.positive;
          default = 3;
          description = "Build attempts before a transient failure becomes permanent.";
        };

        substituteMissEscalationThreshold = lib.mkOption {
          type = lib.types.ints.positive;
          default = 2;
          description = ''
            Free re-queues of a substitutable derivation within one evaluation before it is built
            like any other. A re-queue does not count as a build attempt, so this is the only bound
            on that loop.
          '';
        };

        inputsUnavailableMaxLoops = lib.mkOption {
          type = lib.types.ints.positive;
          default = 3;
          description = ''
            Times a build may retry after missing inputs before it fails instead of retrying again.
          '';
        };

        retryBackoffSecs = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 30;
          description = ''
            Seconds before retrying a transient build failure, doubled for every previous attempt.
          '';
        };

        defaultTimeoutSecs = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 14400;
          description = ''
            Build timeout in seconds for derivations that set no `timeout`. `0` disables it.
          '';
        };

        defaultMaxSilentSecs = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 3600;
          description = ''
            Timeout in seconds without build output for derivations that set no `maxSilent`. `0`
            disables it.
          '';
        };
      };

      scheduler = {
        scoringPolicy = lib.mkOption {
          type = lib.types.enum [ "simple" "resource-aware" ];
          default = "resource-aware";
          description = ''
            Policy ranking queued jobs for a requesting worker. `simple` weighs path availability,
            NAR size, dependency count, waiting time, builtins and fetch worker reservation.
            `resource-aware` also weighs memory fit, worker saturation, CPU, disk and network affinity
            and `preferLocalBuild`.
          '';
        };

        dispatchRetentionDays = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 30;
          description = ''
            Days to keep dispatch records and delivered outbox entries. `0` keeps them forever.
          '';
        };
      };

      metrics = {
        tokenFile = lib.mkOption {
          type = lib.types.nullOr lib.types.path;
          default = null;
          description = ''
            File containing the bearer token required to scrape `GET /metrics`. `null` disables the
            endpoint.
          '';
        };

        rollupIntervalSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 60;
          description = "Seconds between metric rollup runs.";
        };

        retention = {
          rawDays = lib.mkOption {
            type = lib.types.ints.unsigned;
            default = 14;
            description = "Days to keep raw phase and worker samples. `0` keeps them forever.";
          };

          rollupDays = lib.mkOption {
            type = lib.types.ints.unsigned;
            default = 400;
            description = ''
              Days to keep minute and hour rollups; day and week rollups are kept forever. `0` keeps
              everything.
            '';
          };
        };

        labelTopn = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 20;
          description = "Maximum distinct label values per rollup dimension, by activity.";
        };

        cacheFlushIntervalSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 10;
          description = "Seconds between flushes of cache traffic counters to the database.";
        };

        workerSampleIntervalSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 15;
          description = "Seconds between worker metric samples.";
        };

        instanceIntervalSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 30;
          description = "Seconds between recomputations of the instance-wide metric window.";
        };

        graphConsistencyIntervalSecs = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 300;
          description = ''
            Seconds between build graph consistency checks. The check also repairs the NAR reference
            counter, so `0` disables both.
          '';
        };

        otlp = {
          endpoint = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = ''
              OTLP collector endpoint to push metrics to. `null` disables OTLP export.
            '';
          };

          pushIntervalSecs = lib.mkOption {
            type = lib.types.ints.positive;
            default = 30;
            description = "Seconds between OTLP metric pushes.";
          };
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
                  Log level of the cache. `null` uses {option}`services.gradient.log.level.default`.
                '';
              };

              web = lib.mkOption {
                type = lib.types.nullOr logLevelType;
                default = null;
                description = ''
                  Log level of the web API. `null` uses
                  {option}`services.gradient.log.level.default`.
                '';
              };

              proto = lib.mkOption {
                type = lib.types.nullOr logLevelType;
                default = null;
                description = ''
                  Log level of the protocol layer. `null` uses
                  {option}`services.gradient.log.level.default`.
                '';
              };

              scheduler = lib.mkOption {
                type = lib.types.nullOr logLevelType;
                default = null;
                description = ''
                  Log level of the scheduler. `null` uses
                  {option}`services.gradient.log.level.default`.
                '';
              };
            };
          };
          default = { };
          description = "Log levels per component. {env}`RUST_LOG` overrides them at runtime.";
        };

        chunkBytes = lib.mkOption {
          type = lib.types.ints.positive;
          default = 262144;
          description = ''
            Target uncompressed size in bytes of a stored build log chunk. Chunks split on line
            boundaries, so a long line may exceed it.
          '';
        };
      };

      oidc = {
        enable = lib.mkEnableOption "OIDC";
        required = lib.mkEnableOption "OIDC as the only login method";
        clientId = lib.mkOption {
          type = lib.types.str;
          description = "OIDC client ID.";
        };

        clientSecretFile = lib.mkOption {
          type = lib.types.path;
          description = "File containing the OIDC client secret.";
        };

        scopes = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = ["openid" "email" "profile"];
          description = "OIDC scopes to request.";
        };

        discoveryUrl = lib.mkOption {
          type = lib.types.str;
          description = "OIDC discovery URL.";
        };

        iconUrl = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = "URL of the OIDC provider's icon shown on the login page.";
        };
      };

      scim = {
        enable = lib.mkEnableOption "SCIM provisioning";
        tokenFile = lib.mkOption {
          type = lib.types.path;
          description = "File containing the SCIM bearer token.";
        };
        hardDelete = lib.mkEnableOption "deleting users on SCIM `DELETE` instead of disabling them";
      };

      email = {
        enable = lib.mkEnableOption "sending email";
        requireVerification = lib.mkEnableOption "email verification for new accounts";

        smtp = {
          host = lib.mkOption {
            type = lib.types.str;
            description = "SMTP server host name.";
          };

          port = lib.mkOption {
            type = lib.types.port;
            default = 587;
            description = "SMTP server port.";
          };

          username = lib.mkOption {
            type = lib.types.str;
            description = "SMTP user name.";
          };

          passwordFile = lib.mkOption {
            type = lib.types.path;
            description = "File containing the SMTP password.";
          };

          useTls = lib.mkEnableOption "TLS for SMTP";
        };

        from = {
          address = lib.mkOption {
            type = lib.types.str;
            description = "Sender email address.";
          };

          name = lib.mkOption {
            type = lib.types.str;
            default = "Gradient";
            description = "Sender display name.";
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
            File containing the secret that verifies GitHub App webhook payloads. It must match the
            secret set on the App's webhook settings page.
          '';
        };
      };

      s3 = {
        enable = lib.mkEnableOption "storing NARs in S3";
        bucket = lib.mkOption {
          type = lib.types.str;
          default = "";
          description = ''
            Name of the S3 bucket. The bucket must not have versioning, object lock or replication
            enabled: Gradient overwrites objects in place and never removes old versions, so a
            versioned bucket keeps an unreclaimable copy per upload.
          '';
        };

        region = lib.mkOption {
          type = lib.types.str;
          default = "us-east-1";
          description = "Region of the S3 bucket.";
        };

        endpoint = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = ''
            Endpoint of an S3-compatible service such as MinIO or Cloudflare R2. `null` uses AWS.
          '';
        };

        accessKeyId = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = "AWS access key ID. `null` uses instance credentials or the environment.";
        };

        secretAccessKeyFile = lib.mkOption {
          type = lib.types.nullOr lib.types.path;
          default = null;
          description = ''
            File containing the AWS secret access key. `null` uses instance credentials.
          '';
        };

        prefix = lib.mkOption {
          type = lib.types.str;
          default = "";
          description = ''
            Key prefix inside the bucket, such as `gradient/`. Empty stores at the bucket root.
          '';
        };

        virtualHostedStyle = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = ''
            Whether to address a custom {option}`services.gradient.s3.endpoint` virtual-hosted style
            (`https://<bucket>.<endpoint>/key`) instead of path style
            (`https://<endpoint>/<bucket>/key`). MinIO, Garage and most self-hosted services need
            path style. Ignored without a custom endpoint.
          '';
        };

        readTimeoutSecs = lib.mkOption {
          type = lib.types.int;
          default = 60;
          description = ''
            Seconds an S3 response may stall before the request fails. Every received chunk resets
            the timer, so large NARs stream as long as they make progress.
          '';
        };

        maxRetries = lib.mkOption {
          type = lib.types.int;
          default = 3;
          description = "Retries of a failed S3 request.";
        };

        retryTimeoutSecs = lib.mkOption {
          type = lib.types.int;
          default = 250;
          description = ''
            Seconds after the first attempt past which no S3 retry starts. Keep it above
            `(maxRetries + 1) * readTimeoutSecs` so requests failing on the read timeout are still
            retried, and below 5 minutes, since retries reuse the original credentials.
          '';
        };
      };
    };
  };

  config = lib.mkIf cfg.enable {
    services.gradient = lib.mkIf localWorker {
      state.workers.local = {
        display_name = "Local Worker";
        worker_id = localIdentity;
        token_file = localTokenFile;
        projects = [ ];
        base_worker = true;
        authorize_against = localIdentity;
        auto_enable = true;
      };

      worker = {
        id = lib.mkDefault localIdentity;
        peersFile = lib.mkDefault localPeersFile;
        serverUrl = lib.mkDefault "ws://127.0.0.1:${toString cfg.port}/proto";
      };
    };

    assertions = [
      {
        assertion = cfg.localWorker -> cfg.worker.enable;
        message = "services.gradient.localWorker provisions credentials for a worker on this host, so it requires services.gradient.worker.enable.";
      }
      {
        assertion = cfg.proto.federate -> cfg.proto.discoverable;
        message = "proto.federate requires proto.discoverable to be enabled";
      }
      {
        assertion = !(cfg.reverseProxy.nginx.enable && cfg.reverseProxy.caddy.enable);
        message = "You can only use one reverse proxy at a time";
      }
    ];

    # The worker owns its own credential: it generates the token in its own
    # state directory, and the server only ever reads it through
    # LoadCredential, which systemd resolves as root. Ownership is by name, so
    # a switch to DynamicUser would strand these files.
    systemd.services.gradient-local-worker-token = lib.mkIf localWorker {
      description = "Gradient local worker credentials";
      requiredBy = [ "gradient-worker.service" ];
      before = [ "gradient-worker.service" ];

      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        User = "gradient-worker";
        Group = "gradient-worker";
        StateDirectory = "gradient-worker";
        UMask = "0077";
      };

      script = ''
        umask 077
        if [ ! -s ${localTokenFile} ]; then
          ${lib.getExe pkgs.openssl} rand -base64 48 > ${localTokenFile}.new
          mv ${localTokenFile}.new ${localTokenFile}
        fi
        printf '%s:%s\n' ${localIdentity} "$(cat ${localTokenFile})" > ${localPeersFile}.new
        mv ${localPeersFile}.new ${localPeersFile}
        chmod 0400 ${localTokenFile} ${localPeersFile}
      '';
    };

    systemd.services.gradient-worker = lib.mkIf localWorker {
      after = [ "gradient-local-worker-token.service" ];
    };

    systemd.services.gradient-server = {
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
        # Ready only after migrations, which can run for hours; a crash still fails the start at once.
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
        ReadWritePaths = [ cfg.baseDir ];
        Restart = "on-failure";
        RestartSec = 10;
        LimitNOFILE = 65535;
        # Secrets are mlock'd to keep them off swap; without this the lock
        # fails (EPERM) and floods the log on every SSH-key git operation.
        LimitMEMLOCK = "128M";
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
        ] ++ lib.optional cfg.oidc.enable [
          "gradient_oidc_client_secret:${cfg.oidc.clientSecretFile}"
        ] ++ lib.optional cfg.scim.enable [
          "gradient_scim_token:${cfg.scim.tokenFile}"
        ] ++ lib.optional cfg.email.enable [
          "gradient_email_smtp_password:${cfg.email.smtp.passwordFile}"
        ] ++ lib.optionals (cfg.s3.enable && cfg.s3.secretAccessKeyFile != null) [
          "gradient_s3_secret_access_key:${cfg.s3.secretAccessKeyFile}"
        ] ++ lib.optionals cfg.githubApp.enable [
          "gradient_github_app_private_key:${cfg.githubApp.privateKeyFile}"
          "gradient_github_app_webhook_secret:${cfg.githubApp.webhookSecretFile}"
        ] ++ lib.optional (cfg.metrics.tokenFile != null)
          "gradient_metrics_token:${cfg.metrics.tokenFile}"
        ++ userPasswordFiles ++ projectPrivateKeyFiles ++ cacheSigningKeyFiles ++ apiKeyFiles
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
        GRADIENT_LISTEN_ADDR = cfg.listenAddr;
        GRADIENT_PORT = toString cfg.port;
        GRADIENT_SERVE_URL = "http${lib.optionalString cfg.useTls "s"}://${cfg.domain}";
        GRADIENT_FRONTEND_URL = cfg.frontend.url;
        GRADIENT_BASE_DIR = cfg.baseDir;
        GRADIENT_USE_TLS = lib.boolToString cfg.useTls;
        GRADIENT_USE_QUIC = lib.boolToString cfg.useQuic;
        GRADIENT_SECRETS_CRYPT_FILE = "%d/gradient_crypt_secret";
        GRADIENT_SECRETS_JWT_FILE = "%d/gradient_jwt_secret";
        GRADIENT_STATE_FILE = "%d/gradient_state";
        GRADIENT_STATE_DELETE = lib.boolToString cfg.state.delete;
        GRADIENT_CREDENTIALS_DIR = "%d";
        GRADIENT_PERMISSIONS_CREATE_PROJECT = cfg.permissions.createProject;
        GRADIENT_PERMISSIONS_CREATE_CACHE = cfg.permissions.createCache;
        GRADIENT_REGISTRATION_ENABLE = lib.boolToString cfg.registration.enable;
        GRADIENT_SENTRY_ENABLE = lib.boolToString cfg.sentry.enable;
        GRADIENT_DATABASE_URL_FILE = "%d/gradient_database_url";
        GRADIENT_DATABASE_MAX_CONNECTIONS = toString cfg.database.maxConnections;
        GRADIENT_DATABASE_MIN_CONNECTIONS = toString cfg.database.minConnections;
        GRADIENT_DATABASE_CACHE_MAX_CONNECTIONS = toString cfg.database.cache.maxConnections;
        GRADIENT_DATABASE_CACHE_MIN_CONNECTIONS = toString cfg.database.cache.minConnections;
        GRADIENT_DATABASE_WEB_MAX_CONNECTIONS = toString cfg.database.web.maxConnections;
        GRADIENT_DATABASE_WEB_MIN_CONNECTIONS = toString cfg.database.web.minConnections;
        GRADIENT_HTTP_MAX_REQUEST_SIZE = toString cfg.http.maxRequestSize;
        GRADIENT_HTTP_MAX_SOURCE_UPLOAD_SIZE = toString cfg.http.maxSourceUploadSize;
        GRADIENT_HTTP_TRUSTED_PROXIES = builtins.concatStringsSep "," cfg.http.trustedProxies;
        GRADIENT_HTTP_LOCAL_IPS = builtins.concatStringsSep "," cfg.http.localIps;
        GRADIENT_PROTO_DISCOVERABLE = lib.boolToString cfg.proto.discoverable;
        GRADIENT_PROTO_FEDERATE = lib.boolToString cfg.proto.federate;
        GRADIENT_PROTO_MAX_CONNECTIONS = toString cfg.proto.maxConnections;
        GRADIENT_PROTO_WORKER_HEARTBEAT_TIMEOUT_SECS = toString cfg.proto.workerHeartbeatTimeoutSecs;
        GRADIENT_PROTO_ANONYMOUS_CACHE_ENABLE = lib.boolToString cfg.proto.anonymousCache.enable;
        GRADIENT_PROTO_ANONYMOUS_CACHE_MAX_CONNECTIONS_PER_IP = toString cfg.proto.anonymousCache.maxConnectionsPerIp;
        GRADIENT_UPLOAD_CONCURRENCY = toString cfg.upload.concurrency;
        GRADIENT_UPLOAD_BYTES_BUDGET = toString cfg.upload.bytesBudget;
        GRADIENT_UPLOAD_LEASE_IDLE_SECS = toString cfg.upload.leaseIdleSecs;
        GRADIENT_UPLOAD_REST_WAIT_SECS = toString cfg.upload.restWaitSecs;
        GRADIENT_NAR_MAX_UPLOAD_SIZE = toString cfg.nar.maxUploadSize;
        GRADIENT_NAR_SMALL_BYTES = toString cfg.nar.smallBytes;
        GRADIENT_NAR_HOT_CACHE_BYTES = toString cfg.nar.hotCacheBytes;
        GRADIENT_NAR_VERIFY_DIGEST = lib.boolToString cfg.nar.verifyDigest;
        GRADIENT_NAR_STORAGE_OPEN_TIMEOUT_SECS = toString cfg.nar.storageOpenTimeoutSecs;
        GRADIENT_NAR_SEND_CHUNK_TIMEOUT_SECS = toString cfg.nar.sendChunkTimeoutSecs;
        GRADIENT_NAR_MAX_CONCURRENT_SERVES = toString cfg.nar.maxConcurrentServes;
        GRADIENT_NAR_PARTIAL_TTL_SECS = toString cfg.nar.partialTtlSecs;
        GRADIENT_CACHE_UPSTREAM_QUERY_CONCURRENCY = toString cfg.cache.upstreamQueryConcurrency;
        GRADIENT_CACHE_MAX_STORAGE_GB = toString cfg.cache.maxStorageGb;
        GRADIENT_CACHE_SIGN_SWEEP_INTERVAL_SECS = toString cfg.cache.signSweepIntervalSecs;
        GRADIENT_CACHE_DEBUG_INDEX_INTERVAL_SECS = toString cfg.cache.debugIndexIntervalSecs;
        GRADIENT_GC_INTERVAL_SECS = toString cfg.gc.intervalSecs;
        GRADIENT_GC_NAR_TTL_HOURS = toString cfg.gc.narTtlHours;
        GRADIENT_GC_NAR_UPLOAD_GRACE_HOURS = toString cfg.gc.narUploadGraceHours;
        GRADIENT_GC_ORPHAN_DERIVATION_HOURS = toString cfg.gc.orphanDerivationHours;
        GRADIENT_GC_WEDGED_EVAL_HOURS = toString cfg.gc.wedgedEvalHours;
        GRADIENT_EVAL_MAX_KEEP = toString cfg.eval.maxKeep;
        GRADIENT_EVAL_CACHE_MAX_TOTAL_BYTES = toString cfg.eval.cache.maxTotalBytes;
        GRADIENT_EVAL_CACHE_MAX_AGE_DAYS = toString cfg.eval.cache.maxAgeDays;
        GRADIENT_EVAL_CACHE_SWEEP_INTERVAL_SECS = toString cfg.eval.cache.sweepIntervalSecs;
        GRADIENT_BUILD_MAX_ATTEMPTS = toString cfg.build.maxAttempts;
        GRADIENT_BUILD_SUBSTITUTE_MISS_ESCALATION_THRESHOLD = toString cfg.build.substituteMissEscalationThreshold;
        GRADIENT_BUILD_INPUTS_UNAVAILABLE_MAX_LOOPS = toString cfg.build.inputsUnavailableMaxLoops;
        GRADIENT_BUILD_RETRY_BACKOFF_SECS = toString cfg.build.retryBackoffSecs;
        GRADIENT_BUILD_DEFAULT_TIMEOUT_SECS = toString cfg.build.defaultTimeoutSecs;
        GRADIENT_BUILD_DEFAULT_MAX_SILENT_SECS = toString cfg.build.defaultMaxSilentSecs;
        GRADIENT_SCHEDULER_SCORING_POLICY = cfg.scheduler.scoringPolicy;
        GRADIENT_SCHEDULER_DISPATCH_RETENTION_DAYS = toString cfg.scheduler.dispatchRetentionDays;
        GRADIENT_METRICS_ROLLUP_INTERVAL_SECS = toString cfg.metrics.rollupIntervalSecs;
        GRADIENT_METRICS_RETENTION_RAW_DAYS = toString cfg.metrics.retention.rawDays;
        GRADIENT_METRICS_RETENTION_ROLLUP_DAYS = toString cfg.metrics.retention.rollupDays;
        GRADIENT_METRICS_LABEL_TOPN = toString cfg.metrics.labelTopn;
        GRADIENT_METRICS_CACHE_FLUSH_INTERVAL_SECS = toString cfg.metrics.cacheFlushIntervalSecs;
        GRADIENT_METRICS_WORKER_SAMPLE_INTERVAL_SECS = toString cfg.metrics.workerSampleIntervalSecs;
        GRADIENT_METRICS_INSTANCE_INTERVAL_SECS = toString cfg.metrics.instanceIntervalSecs;
        GRADIENT_METRICS_GRAPH_CONSISTENCY_INTERVAL_SECS = toString cfg.metrics.graphConsistencyIntervalSecs;
        GRADIENT_METRICS_OTLP_PUSH_INTERVAL_SECS = toString cfg.metrics.otlp.pushIntervalSecs;
        GRADIENT_LOG_LEVEL_DEFAULT = cfg.log.level.default;
        GRADIENT_LOG_CHUNK_BYTES = toString cfg.log.chunkBytes;
        GRADIENT_OIDC_ENABLE = lib.boolToString cfg.oidc.enable;
        GRADIENT_SCIM_ENABLE = lib.boolToString cfg.scim.enable;
      } // lib.optionalAttrs (cfg.pullRequests.commitName != null) {
        GRADIENT_PULL_REQUESTS_COMMIT_NAME = cfg.pullRequests.commitName;
      } // lib.optionalAttrs (cfg.pullRequests.commitEmail != null) {
        GRADIENT_PULL_REQUESTS_COMMIT_EMAIL = cfg.pullRequests.commitEmail;
      } // lib.optionalAttrs (cfg.sentry.dsn != null) {
        GRADIENT_SENTRY_DSN = cfg.sentry.dsn;
      } // lib.optionalAttrs (cfg.log.level.cache != null) {
        GRADIENT_LOG_LEVEL_CACHE = cfg.log.level.cache;
      } // lib.optionalAttrs (cfg.log.level.web != null) {
        GRADIENT_LOG_LEVEL_WEB = cfg.log.level.web;
      } // lib.optionalAttrs (cfg.log.level.proto != null) {
        GRADIENT_LOG_LEVEL_PROTO = cfg.log.level.proto;
      } // lib.optionalAttrs (cfg.log.level.scheduler != null) {
        GRADIENT_LOG_LEVEL_SCHEDULER = cfg.log.level.scheduler;
      } // lib.optionalAttrs cfg.oidc.enable {
        GRADIENT_OIDC_CLIENT_ID = cfg.oidc.clientId;
        GRADIENT_OIDC_CLIENT_SECRET_FILE = "%d/gradient_oidc_client_secret";
        GRADIENT_OIDC_SCOPES = builtins.concatStringsSep " " cfg.oidc.scopes;
        GRADIENT_OIDC_DISCOVERY_URL = cfg.oidc.discoveryUrl;
        GRADIENT_OIDC_REQUIRED = lib.boolToString cfg.oidc.required;
      } // lib.optionalAttrs cfg.scim.enable {
        GRADIENT_SCIM_TOKEN_FILE = "%d/gradient_scim_token";
        GRADIENT_SCIM_HARD_DELETE = lib.boolToString cfg.scim.hardDelete;
      } // lib.optionalAttrs cfg.email.enable {
        GRADIENT_EMAIL_ENABLE = lib.boolToString cfg.email.enable;
        GRADIENT_EMAIL_REQUIRE_VERIFICATION = lib.boolToString cfg.email.requireVerification;
        GRADIENT_EMAIL_SMTP_HOST = cfg.email.smtp.host;
        GRADIENT_EMAIL_SMTP_PORT = toString cfg.email.smtp.port;
        GRADIENT_EMAIL_SMTP_USERNAME = cfg.email.smtp.username;
        GRADIENT_EMAIL_SMTP_PASSWORD_FILE = "%d/gradient_email_smtp_password";
        GRADIENT_EMAIL_SMTP_USE_TLS = lib.boolToString cfg.email.smtp.useTls;
        GRADIENT_EMAIL_FROM_ADDRESS = cfg.email.from.address;
        GRADIENT_EMAIL_FROM_NAME = cfg.email.from.name;
      } // lib.optionalAttrs cfg.s3.enable {
        GRADIENT_S3_BUCKET = cfg.s3.bucket;
        GRADIENT_S3_REGION = cfg.s3.region;
        GRADIENT_S3_PREFIX = cfg.s3.prefix;
        GRADIENT_S3_VIRTUAL_HOSTED_STYLE = lib.boolToString cfg.s3.virtualHostedStyle;
        GRADIENT_S3_READ_TIMEOUT_SECS = toString cfg.s3.readTimeoutSecs;
        GRADIENT_S3_MAX_RETRIES = toString cfg.s3.maxRetries;
        GRADIENT_S3_RETRY_TIMEOUT_SECS = toString cfg.s3.retryTimeoutSecs;
      } // lib.optionalAttrs (cfg.s3.enable && cfg.s3.endpoint != null) {
        GRADIENT_S3_ENDPOINT = cfg.s3.endpoint;
      } // lib.optionalAttrs (cfg.s3.enable && cfg.s3.accessKeyId != null) {
        GRADIENT_S3_ACCESS_KEY_ID = cfg.s3.accessKeyId;
      } // lib.optionalAttrs (cfg.s3.enable && cfg.s3.secretAccessKeyFile != null) {
        GRADIENT_S3_SECRET_ACCESS_KEY_FILE = "%d/gradient_s3_secret_access_key";
      } // lib.optionalAttrs cfg.githubApp.enable {
        GRADIENT_GITHUB_APP_ID = toString cfg.githubApp.id;
        GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE = "%d/gradient_github_app_private_key";
        GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE = "%d/gradient_github_app_webhook_secret";
      } // lib.optionalAttrs (cfg.metrics.tokenFile != null) {
        GRADIENT_METRICS_TOKEN_FILE = "%d/gradient_metrics_token";
      } // lib.optionalAttrs (cfg.metrics.otlp.endpoint != null) {
        GRADIENT_METRICS_OTLP_ENDPOINT = cfg.metrics.otlp.endpoint;
      };
    };

    services = {
      nginx = lib.mkIf cfg.reverseProxy.nginx.enable {
        enable = true;
        virtualHosts."${cfg.domain}" = {
          enableACME = cfg.useTls && cfg.reverseProxy.nginx.manageTls;
          forceSSL = cfg.useTls && cfg.reverseProxy.nginx.manageTls;
          http2 = true;
          http3 = cfg.useQuic;
          locations = {
            "/" = lib.mkIf cfg.frontend.enable {
              root = "${cfg.packages.frontend}/share/gradient-frontend";
              tryFiles = "$uri $uri/ /index.html";
            };

            "/api/" = {
              proxyPass = "http://${config.services.gradient.listenAddr}:${toString config.services.gradient.port}";
              proxyWebsockets = true;
              # NAR and blob uploads are streamed straight to gradient: buffering
              # them would spool the whole body to /tmp before we see a byte, and
              # log endpoints stream their response the same way.
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
              # An upgraded connection is relayed through a buffer sized by
              # proxy_buffer_size, which defaults to a single page. NAR chunks
              # are 512 KiB, so the default turns one frame into hundreds of
              # read/write pairs inside nginx. proxy_buffers has to move with
              # it: nginx derives proxy_busy_buffers_size as twice the larger
              # of the two and rejects the config unless that still fits in
              # proxy_buffers minus one buffer, even though an upgraded
              # connection never allocates them.
              extraConfig = ''
                proxy_buffer_size 256k;
                proxy_buffers 4 256k;
                proxy_connect_timeout 90d;
                proxy_send_timeout 90d;
                proxy_read_timeout 90d;
              '';
            };

            # Regex, so it wins over the "/cache/" prefix: this is the only
            # upgraded connection under it, and it is the only one that wants a
            # relay buffer sized for 4 MiB NAR chunks. Widening "/cache/"
            # instead would cost that much memory per in-flight NAR download.
            "~ ^/cache/[^/]+/proto$" = {
              proxyPass = "http://${config.services.gradient.listenAddr}:${toString config.services.gradient.port}";
              proxyWebsockets = true;
              extraConfig = ''
                proxy_buffer_size 256k;
                proxy_buffers 4 256k;
                proxy_connect_timeout 1h;
                proxy_send_timeout 1h;
                proxy_read_timeout 1h;
              '';
            };

            "/cache/" = {
              proxyPass = "http://${config.services.gradient.listenAddr}:${toString config.services.gradient.port}";
              proxyWebsockets = true;
              # A substituter pulls NARs that run to hundreds of MB. With the
              # default buffering nginx writes each one to proxy_temp_path first,
              # which fills the disk and kills the transfer mid-stream.
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
          # No counterpart to the nginx relay-buffer tuning: Caddy tunnels an
          # upgraded connection bidirectionally with no intermediate buffer to
          # size, and its request/response buffering is off by default, which
          # is what streaming NARs want.
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
