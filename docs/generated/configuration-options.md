## General

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `baseDir` | path | `"/var/lib/gradient"` | `GRADIENT_BASE_DIR` | Directory holding Gradient's state, NAR files and caches. |
| `domain` | string | - | - | Domain under which Gradient is reachable. |
| `enable` | bool | `false` | - | Whether to enable Gradient, a Nix-CI for Teams. |
| `environmentVariables` | lazy attribute set of (null or bool or int or string or path) | `{ }` | - | Environment variables of the Gradient server. The [configuration reference](https://wavelens.github.io/gradient/reference/configuration) lists every variable and its default. Set a variable to `null` to unset it. Secrets belong into `secrets`. |
| `listenAddr` | string | `"127.0.0.1"` | `GRADIENT_LISTEN_ADDR` | IP address for the Gradient server to listen on. |
| `localWorker` | bool | `worker.enable` | - | Whether to register the `worker` on this host automatically, with an ID from the hostname and a token and peers file from the first start. The state-declared team `server` is part of every new project. |
| `port` | port | `3000` | `GRADIENT_PORT` | Port for the Gradient server to listen on. |
| `publicStats` | bool | `false` | `GRADIENT_PUBLIC_STATS` | Expose `GET /api/v1/workers` and worker stats without authentication. Only superusers can access those endpoints with `false` (default). |
| `retentionDays` | unsigned int | `90` | `GRADIENT_RETENTION_DAYS` | Days to keep job assignment records, finished deliveries, worker connection history, webhook and task action deliveries, expired sessions and CLI logins. Finished admin tasks, the audit log, per-build resource samples and finished cluster jobs share the same limit. Removed resource samples no longer feed build predictions. Finished cluster jobs without remaining members go on the next hourly pass. Active cluster jobs and the newest finished admin task of each kind stay exempt. Open worker connections stay until the next connection of the same worker. Set it to `0` to keep every record forever. |
| `serveUrl` | string | derived | `GRADIENT_SERVE_URL` | Public URL of Gradient. Set this for a URL other than `domain`, for example behind a port mapping. |
| `storePath` | null or string | `null` | `GRADIENT_STORE_PATH` |  |
| `useQuic` | bool | `false` | `GRADIENT_USE_QUIC` | Whether to enable advertising HTTP/3 (QUIC) to clients. |
| `useTls` | bool | `true` | `GRADIENT_USE_TLS` | Whether to enable TLS. |

## `build`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `build.defaultMaxSilentSecs` | unsigned int | `3600` | `GRADIENT_BUILD_DEFAULT_MAX_SILENT_SECS` | Timeout in seconds without build output for derivations without `maxSilent`. Set it to `0` to disable the timeout. |
| `build.defaultTimeoutSecs` | unsigned int | `14400` | `GRADIENT_BUILD_DEFAULT_TIMEOUT_SECS` | Build timeout in seconds for derivations without `timeout`. Set it to `0` to disable the timeout. |
| `build.inputsUnavailableMaxLoops` | positive int | `3` | `GRADIENT_BUILD_INPUTS_UNAVAILABLE_MAX_LOOPS` | Times a build can retry after missing inputs before failing instead of retrying again. |
| `build.maxAttempts` | positive int | `3` | `GRADIENT_BUILD_MAX_ATTEMPTS` | Build or eval job attempts before a transient failure is permanent. |
| `build.retryBackoffSecs` | unsigned int | `30` | `GRADIENT_BUILD_RETRY_BACKOFF_SECS` | Seconds before retrying a transient build failure, doubled for every previous attempt. |
| `build.substituteMissEscalationThreshold` | positive int | `2` | `GRADIENT_BUILD_SUBSTITUTE_MISS_ESCALATION_THRESHOLD` | Free re-queues of a derivation available in a cache within one evaluation, before building the derivation like any other. A re-queue does not count as a build attempt. This threshold is the only bound on that loop. |

## `cache`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `cache.debugIndexIntervalSecs` | positive int | `300` | `GRADIENT_CACHE_DEBUG_INDEX_INTERVAL_SECS` | Seconds between passes of the DWARF build ID index backfill. Uploads enter the index immediately. Only paths cached before the index existed or interrupted by a restart need the pass. |
| `cache.maxStorageGb` | unsigned int | `0` | `GRADIENT_CACHE_MAX_STORAGE_GB` | Instance-wide limit on cached NAR storage in GB. New evaluations wait while every writable cache of a project has less than 10 MiB left. Set it to `0` to disable the limit. Per-cache limits still apply. |
| `cache.signSweepIntervalSecs` | positive int | `3600` | `GRADIENT_CACHE_SIGN_SWEEP_INTERVAL_SECS` | Seconds between NAR signature backfill passes. Uploads receive their signature immediately. Only subscription placeholders and unsigned leftovers need the pass. |
| `cache.upstreamQueryConcurrency` | positive int | `32` | `GRADIENT_CACHE_UPSTREAM_QUERY_CONCURRENCY` | Maximum simultaneous narinfo requests to upstream caches across the server. |

## `database`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `database.cache.maxConnections` | positive int | `32` | `GRADIENT_DATABASE_CACHE_MAX_CONNECTIONS` | Maximum connections of the cache query pool. It is separate from the scheduler pool to keep a large evaluation's prefetch traffic from starving job assignment. |
| `database.cache.minConnections` | unsigned int | `2` | `GRADIENT_DATABASE_CACHE_MIN_CONNECTIONS` | Minimum connections kept open in the cache query pool. |
| `database.maxConnections` | positive int | `16` | `GRADIENT_DATABASE_MAX_CONNECTIONS` | Maximum connections of the scheduler and worker pool. Server processes each open up to the sum of `database.maxConnections`, `database.cache.maxConnections` and `database.web.maxConnections`. PostgreSQL's `max_connections` must accommodate that sum. |
| `database.minConnections` | unsigned int | `2` | `GRADIENT_DATABASE_MIN_CONNECTIONS` | Minimum connections kept open in the scheduler and worker pool. |
| `database.url` | string | `"postgresql://gradient@localhost/gradient?host=/run/postgresql"` | `GRADIENT_DATABASE_URL` | PostgreSQL connection URL. |
| `database.urlFile` | path | derived | `GRADIENT_DATABASE_URL_FILE` | File containing the PostgreSQL connection URL, taking precedence over `database.url`. |
| `database.web.maxConnections` | positive int | `8` | `GRADIENT_DATABASE_WEB_MAX_CONNECTIONS` | Maximum connections of the HTTP API pool. |
| `database.web.minConnections` | unsigned int | `1` | `GRADIENT_DATABASE_WEB_MIN_CONNECTIONS` | Minimum connections kept open in the HTTP API pool. |

## `email`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `email.enable` | bool | `false` | `GRADIENT_EMAIL_ENABLE` | Whether to enable sending email. |
| `email.from.address` | string | - | `GRADIENT_EMAIL_FROM_ADDRESS` | Sender email address. |
| `email.from.name` | string | `"Gradient"` | `GRADIENT_EMAIL_FROM_NAME` | Sender display name. |
| `email.requireVerification` | bool | `false` | `GRADIENT_EMAIL_REQUIRE_VERIFICATION` | Whether to enable email verification for new accounts. |
| `email.smtp.host` | string | - | `GRADIENT_EMAIL_SMTP_HOST` | SMTP server host name. |
| `email.smtp.passwordFile` | path | - | `GRADIENT_EMAIL_SMTP_PASSWORD_FILE` | File containing the SMTP password. |
| `email.smtp.port` | port | `587` | `GRADIENT_EMAIL_SMTP_PORT` | SMTP server port. |
| `email.smtp.useTls` | bool | `true` | `GRADIENT_EMAIL_SMTP_USE_TLS` | Whether to enable TLS for SMTP. |
| `email.smtp.username` | string | - | `GRADIENT_EMAIL_SMTP_USERNAME` | SMTP user name. |

## `eval`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `eval.cache.maxAgeDays` | unsigned int | `30` | `GRADIENT_EVAL_CACHE_MAX_AGE_DAYS` | Days since the last compaction after which the eval cache of a flake fingerprint leaves with all its segments, regardless of the size limit. |
| `eval.cache.maxTotalBytes` | unsigned int | `10737418240` | `GRADIENT_EVAL_CACHE_MAX_TOTAL_BYTES` | Total size in bytes of the shared eval caches, counted after compaction. The eval cache of a flake fingerprint leaves with all its segments, least recently compacted first, until the total is within this size. |
| `eval.cache.sweepIntervalSecs` | positive int | `3600` | `GRADIENT_EVAL_CACHE_SWEEP_INTERVAL_SECS` | Seconds between eval cache eviction passes. |
| `eval.maxKeep` | unsigned int | `30` | `GRADIENT_EVAL_MAX_KEEP` | Maximum number of evaluations kept per task. It is the cap for the per-task setting, and new tasks start at the lower of 30 and this value. Set it to `0` to disable the limit. |

## `exec`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `exec.enable` | bool | `true` | `GRADIENT_EXEC_ENABLE` | Whether to enable exec jobs. When disabled, exec entry points fail with a message instead of starting a job. |

## `frontend`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `frontend.enable` | bool | `true` | - | Whether to enable the Gradient web frontend. |
| `frontend.logoUrl` | null or string | `null` | `GRADIENT_FRONTEND_LOGO_URL` | URL of an image to display instead of the Gradient logo on the sign-in pages, in its original colors. Set it to `null` to keep the Gradient logo. |
| `frontend.url` | string | `serveUrl` | `GRADIENT_FRONTEND_URL` | Public URL of the Gradient frontend, used for links in CI status reports. |

## `gc`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `gc.deepIntervalSecs` | unsigned int | `3600` | `GRADIENT_GC_DEEP_INTERVAL_SECS` | Seconds from the end of one background deep garbage collection round to the start of the next. Set it to `0` to run rounds only on request. |
| `gc.deepPaceMs` | unsigned int | `1000` | `GRADIENT_GC_DEEP_PACE_MS` | Milliseconds between two units of a storage migration or a background deep garbage collection round. Requested rounds run their units without a pause. |
| `gc.intervalSecs` | positive int | `3600` | `GRADIENT_GC_INTERVAL_SECS` | Seconds between garbage collection passes. |
| `gc.narTtlHours` | unsigned int | `336` | `GRADIENT_GC_NAR_TTL_HOURS` | Hours to keep a cached path outside the live closure of retained evaluations after its last fetch, or its upload if never fetched. These hours always come on top of `gc.narUploadGraceHours`. Set it to `0` to keep nothing beyond that grace. |
| `gc.narUploadGraceHours` | unsigned int | `24` | `GRADIENT_GC_NAR_UPLOAD_GRACE_HOURS` | Hours before the deletion of an unreferenced NAR object, covering the window between its upload and the commit of its database rows. |
| `gc.orphanDerivationHours` | unsigned int | `24` | `GRADIENT_GC_ORPHAN_DERIVATION_HOURS` | Hours before the deletion of a derivation outside the build closure of every retained evaluation. Quick re-evaluations can reuse the derivation during this grace. Set it to `0` to delete such derivations on the next run. |
| `gc.wedgedEvalHours` | unsigned int | `24` | `GRADIENT_GC_WEDGED_EVAL_HOURS` | Hours an evaluation can stay in one phase before counting as stuck. Stuck evaluations no longer block evaluation garbage collection. Set it to `0` to treat no evaluation as stuck. |

## `githubApp`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `githubApp.enable` | bool | `false` | - | Whether to enable the GitHub App integration for webhooks and CI status reports. |
| `githubApp.id` | positive int | - | `GRADIENT_GITHUB_APP_ID` | GitHub App ID, shown on the App's settings page. |
| `githubApp.privateKeyFile` | path | - | `GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE` | File containing the GitHub App's RS256 private key in PEM format. |
| `githubApp.webhookSecretFile` | path | - | `GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE` | File containing the secret for verifying GitHub App webhook payloads. It must match the secret set on the App's webhook settings page. |

## `gradientCi`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `gradientCi.enable` | bool | `true` | `GRADIENT_GRADIENT_CI_ENABLE` | Whether to enable the Gradient.CI Servers offer on the workers pages. |

## `http`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `http.cacheRequestBurst` | positive int | `20000` | `GRADIENT_HTTP_CACHE_REQUEST_BURST` | Requests each client IP can make to the binary cache in a burst before `http.cacheRequestsPerSecond` applies. |
| `http.cacheRequestsPerSecond` | positive int | `1000` | `GRADIENT_HTTP_CACHE_REQUESTS_PER_SECOND` | Sustained requests per second each client IP can make to the binary cache (`narinfo`, NAR and cache proto endpoints). Nix asks for one narinfo per path of a closure. |
| `http.localIps` | list of string | `[ "192.168.0.0/16" "172.16.0.0/12" "100.64.0.0/10" "10.0.0.0/8" "fc00::/7" ]` | `GRADIENT_HTTP_LOCAL_IPS` | Clients in these CIDR ranges receive a cache's `local_priority`, if set and non-zero. |
| `http.maxRequestSize` | positive int | `2097152` | `GRADIENT_HTTP_MAX_REQUEST_SIZE` | Maximum HTTP request body size in bytes for most endpoints, to keep an unbounded body from exhausting server memory. Build request blob uploads use a fixed 20 MiB cap. |
| `http.maxSourceUploadSize` | positive int | `536870912` | `GRADIENT_HTTP_MAX_SOURCE_UPLOAD_SIZE` | Maximum size in bytes of a source upload to `POST /build-requests/source` (as sent by `gradient build`) and of a chunked manifest in total. The built-in reverse proxies raise their body size limit to fit it. |
| `http.trustedProxies` | list of string | `[ "127.0.0.1/8" "::1/128" ]` | `GRADIENT_HTTP_TRUSTED_PROXIES` | CIDR ranges of peers allowed to set `X-Forwarded-For`. The defaults trust a reverse proxy on the same host. |

## `log`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `log.chunkBytes` | positive int | `262144` | `GRADIENT_LOG_CHUNK_BYTES` | Target uncompressed size in bytes of a stored build log chunk. Chunks split on line boundaries, and a long line can exceed the target. |
| `log.level.cache` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_LOG_LEVEL_CACHE` | Log level of the cache. Set it to `null` to use `log.level.default`. |
| `log.level.default` | one of `trace` `debug` `info` `warn` `error` | `"info"` | `GRADIENT_LOG_LEVEL_DEFAULT` | Default log level. |
| `log.level.proto` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_LOG_LEVEL_PROTO` | Log level of the protocol layer. Set it to `null` to use `log.level.default`. |
| `log.level.scheduler` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_LOG_LEVEL_SCHEDULER` | Log level of the scheduler. Set it to `null` to use `log.level.default`. |
| `log.level.web` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_LOG_LEVEL_WEB` | Log level of the web API. Set it to `null` to use `log.level.default`. |
| `log.traceDir` | null or path | `null` | `GRADIENT_LOG_TRACE_DIR` | Directory receiving every closed stage span of the server as JSON lines, one file per process. Set it to `null` to disable span tracing. |

## `metrics`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `metrics.cacheFlushIntervalSecs` | positive int | `10` | `GRADIENT_METRICS_CACHE_FLUSH_INTERVAL_SECS` | Seconds between flushes of cache traffic counters to the database. |
| `metrics.graphConsistencyIntervalSecs` | unsigned int | `300` | `GRADIENT_METRICS_GRAPH_CONSISTENCY_INTERVAL_SECS` | Seconds between build graph consistency checks. These checks also repair the NAR reference counter. Set it to `0` to disable both. |
| `metrics.instanceIntervalSecs` | positive int | `30` | `GRADIENT_METRICS_INSTANCE_INTERVAL_SECS` | Seconds between updates of the instance-wide metric window. |
| `metrics.labelTopn` | unsigned int | `20` | `GRADIENT_METRICS_LABEL_TOPN` | Maximum distinct label values per rollup dimension, by activity. |
| `metrics.otlp.endpoint` | null or string | `null` | `GRADIENT_METRICS_OTLP_ENDPOINT` | OTLP collector endpoint to push metrics to. Set it to `null` to disable OTLP export. |
| `metrics.otlp.pushIntervalSecs` | positive int | `30` | `GRADIENT_METRICS_OTLP_PUSH_INTERVAL_SECS` | Seconds between OTLP metric pushes. |
| `metrics.retention.rawDays` | unsigned int | `14` | `GRADIENT_METRICS_RETENTION_RAW_DAYS` | Days to keep raw phase and worker samples and the per-minute cache and upstream traffic counters. Set it to `0` to keep them forever. |
| `metrics.retention.rollupDays` | unsigned int | `400` | `GRADIENT_METRICS_RETENTION_ROLLUP_DAYS` | Days to keep minute and hour rollups. Day and week rollups stay forever. Set it to `0` to keep every rollup forever. |
| `metrics.rollupIntervalSecs` | positive int | `60` | `GRADIENT_METRICS_ROLLUP_INTERVAL_SECS` | Seconds between metric rollup passes. |
| `metrics.tokenFile` | null or path | `null` | `GRADIENT_METRICS_TOKEN_FILE` | File containing the bearer token required to scrape `GET /metrics`. Set it to `null` to disable the endpoint. |
| `metrics.workerSampleIntervalSecs` | positive int | `15` | `GRADIENT_METRICS_WORKER_SAMPLE_INTERVAL_SECS` | Seconds between worker metric samples. |

## `nar`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `nar.chunkBytes` | int from 4096 to 4194304 | `524288` | `GRADIENT_NAR_CHUNK_BYTES` | Size in bytes of each outbound `NarPush` chunk, from 4 KiB to 4 MiB. Smaller chunks can help slow or high-latency links like Tailscale over DERP. |
| `nar.hotCacheBytes` | unsigned int | `536870912` | `GRADIENT_NAR_HOT_CACHE_BYTES` | Capacity in bytes of the in-memory NAR cache. Set it to `0` to disable the cache. |
| `nar.maxConcurrentDownloads` | positive int | `16` | `GRADIENT_NAR_MAX_CONCURRENT_DOWNLOADS` | NAR downloads from storage that can run at once across all connections, keeping the storage near its best total throughput. |
| `nar.maxConcurrentServes` | positive int | `8` | `GRADIENT_NAR_MAX_CONCURRENT_SERVES` | NAR serving tasks that can run at once per worker connection, bounding memory and storage fan-out for large batches. |
| `nar.maxUploadSize` | positive int | `536870912` | `GRADIENT_NAR_MAX_UPLOAD_SIZE` | Maximum size in bytes of a NAR uploaded to the cache upload endpoint. |
| `nar.partialTtlSecs` | unsigned int | `86400` | `GRADIENT_NAR_PARTIAL_TTL_SECS` | Maximum age in seconds of an unfinished upload staged under `<baseDir>`, counted from its last write. Older uploads disappear in the next deep GC. Set it to `0` to keep every unfinished upload. |
| `nar.sendChunkTimeoutSecs` | positive int | `30` | `GRADIENT_NAR_SEND_CHUNK_TIMEOUT_SECS` | Seconds an outbound `NarPush` chunk can wait for the WebSocket to drain before an abort of the transfer with `NarAbort`. |
| `nar.smallBytes` | unsigned int | `1048576` | `GRADIENT_NAR_SMALL_BYTES` | Size in bytes up to which NAR downloads come from the server itself instead of a presigned S3 URL. NARs up to this size also stay in the in-memory cache. Uploads do not depend on this value. |
| `nar.storageOpenTimeoutSecs` | positive int | `60` | `GRADIENT_NAR_STORAGE_OPEN_TIMEOUT_SECS` | Seconds to wait for a NAR object stream from storage (for example an S3 GET) before answering the worker with `NarAbort`. Workers retry after a `NarAbort`. |
| `nar.verifyDigest` | bool | `false` | `GRADIENT_NAR_VERIFY_DIGEST` | Whether to download NARs committed through presigned S3 uploads and verify their hash, catching same-length corruption at the cost of a full object read. Size checks run either way. Passthrough and REST uploads always go through verification. |

## `oidc`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `oidc.clientId` | string | - | `GRADIENT_OIDC_CLIENT_ID` | OIDC client ID. |
| `oidc.clientSecretFile` | path | - | `GRADIENT_OIDC_CLIENT_SECRET_FILE` | File containing the OIDC client secret. |
| `oidc.discoveryUrl` | string | - | `GRADIENT_OIDC_DISCOVERY_URL` | OIDC discovery URL. |
| `oidc.enable` | bool | `false` | `GRADIENT_OIDC_ENABLE` | Whether to enable OIDC. |
| `oidc.required` | bool | `false` | `GRADIENT_OIDC_REQUIRED` | Whether to enable OIDC as the only login method. |
| `oidc.scopes` | list of string | `[ "openid" "email" "profile" ]` | `GRADIENT_OIDC_SCOPES` | OIDC scopes to request. |

## `packages`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `packages.frontend` | package | derived | - | The gradient-frontend package to use. |
| `packages.server` | package | derived | - | The gradient package to use. |

## `permissions`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `permissions.createCache` | one of `none` `superusers` `everyone` | `"everyone"` | `GRADIENT_PERMISSIONS_CREATE_CACHE` | Who can create caches through the API: `none` (only the declarative state), `superusers`, or `everyone` (any authenticated user). |
| `permissions.createProject` | one of `none` `superusers` `everyone` | `"everyone"` | `GRADIENT_PERMISSIONS_CREATE_PROJECT` | Who can create projects through the API: `none` (only the declarative state), `superusers`, or `everyone` (any authenticated user). |
| `permissions.createTeam` | one of `none` `superusers` `everyone` | `"everyone"` | `GRADIENT_PERMISSIONS_CREATE_TEAM` | Who can create teams through the API: `none` (only the declarative state), `superusers`, or `everyone` (any authenticated user). |

## `postgres`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `postgres.effectiveCacheSize` | null or string | `null` | - | `effective_cache_size` of the cluster set up by `postgres.enable`. Set it to three quarters of the host's RAM. It is a planner hint, not an allocation. Set it to `null` to keep the PostgreSQL default. |
| `postgres.enable` | bool | `false` | - | Whether to enable a local PostgreSQL database for Gradient. |
| `postgres.maintenanceWorkMem` | null or string | `null` | - | `maintenance_work_mem` of the cluster set up by `postgres.enable`, used for building indexes and by autovacuum. Each of `autovacuum_max_workers` can claim this much at once. Set it to `null` to keep the PostgreSQL default. |
| `postgres.sharedBuffers` | null or string | `"512MB"` | - | `shared_buffers` of the cluster set up by `postgres.enable`. Size it to a quarter of the host's RAM. Gradient's working set is the build graph's indexes, and the stock 128 MB cannot keep them resident. This value is a single fixed allocation, unlike `work_mem` and `maintenance_work_mem`. The default is a quarter of the smallest supported host. Raise it on larger hosts. Set it to `null` to keep the PostgreSQL default. |
| `postgres.workMem` | null or string | `null` | - | `work_mem` of the cluster set up by `postgres.enable`. Every sort or hash node in a query can claim this much. The scheduler's graph walks set a higher per-statement limit, except walks seeded by a root array, which keep the PostgreSQL default. Use `"32MB"` on a host sized for the three server pools; that is too much for a small host. `null` keeps the PostgreSQL default. |

## `proto`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `proto.anonymousCache.enable` | bool | `true` | `GRADIENT_PROTO_ANONYMOUS_CACHE_ENABLE` | Whether unauthenticated clients can use `GET /cache/{cache}/proto` for public caches. Private caches always require an API key. |
| `proto.anonymousCache.maxConnectionsPerIp` | positive int | `32` | `GRADIENT_PROTO_ANONYMOUS_CACHE_MAX_CONNECTIONS_PER_IP` | Maximum simultaneous anonymous `/cache/proto` connections per client IP. |
| `proto.assignmentReclaimGraceSecs` | unsigned int | `600` | `GRADIENT_PROTO_ASSIGNMENT_RECLAIM_GRACE_SECS` | Seconds the server keeps the open assignments of a disconnected worker for the worker to report them in `RunningJobs`. After a server restart this covers the open assignments of every worker. Assignments nobody reports within this time are re-queued and count as a lost worker. A worker that disconnected with a `maxLifetime` gets at most its remaining lifetime. Assignments kept over a server restart get the full time. Set it to `0` to re-queue at once. |
| `proto.discoverable` | bool | `true` | `GRADIENT_PROTO_DISCOVERABLE` | Whether to enable incoming worker and federation connections on `/proto`. |
| `proto.federate` | bool | `false` | `GRADIENT_PROTO_FEDERATE` | Whether to enable federation with other Gradient servers over `/proto`. |
| `proto.maxConnections` | positive int | `256` | `GRADIENT_PROTO_MAX_CONNECTIONS` | Maximum simultaneous `/proto` WebSocket connections. |
| `proto.public` | bool | `false` | - | Whether to enable exposing `/proto` through the reverse proxy for remote workers and federation. |
| `proto.workerHeartbeatTimeoutSecs` | unsigned int | `120` | `GRADIENT_PROTO_WORKER_HEARTBEAT_TIMEOUT_SECS` | Seconds a connected worker can stay silent before counting as dead, with its jobs re-queued. Workers heartbeat every 10 seconds, giving the default a tolerance of twelve missed beats. The timeout is the only detection for a worker lost without a clean TCP close (OOM kill, frozen host, network partition). Set it to `0` to disable the watchdog. |

## `pullRequests`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `pullRequests.commitEmail` | null or string | `null` | `GRADIENT_PULL_REQUESTS_COMMIT_EMAIL` | Git author and committer email for commits of the `open_pr` action. Set it to `null` to use the default of the Git host, as listed for `pullRequests.commitName`. |
| `pullRequests.commitName` | null or string | `null` | `GRADIENT_PULL_REQUESTS_COMMIT_NAME` | Git author and committer name for commits of the `open_pr` action. Set it to `null` to use the default of the Git host. - GitHub: the App bot, with the commit marked as verified. - Gitea, Forgejo and GitLab: the token owner, with `Gradient <gradient@users.noreply.HOST>` as fallback. Grant the token the `read:user` or `read_user` scope for the owner lookup. |

## `registration`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `registration.enable` | bool | `true` | `GRADIENT_REGISTRATION_ENABLE` | Whether to enable self-service user registration. |

## `reverseProxy`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `reverseProxy.caddy.enable` | bool | `false` | - | Whether to enable a Caddy virtual host for Gradient. |
| `reverseProxy.caddy.extraConfig` | strings concatenated with "\n" | `""` | - | Additional lines appended to `services.caddy.virtualHosts.<name>.extraConfig` after the reverse proxy setup. |
| `reverseProxy.caddy.useACMEHost` | null or string | `null` | - | Host of an existing ACME certificate, passed to `services.caddy.virtualHosts.<name>.useACMEHost`. Caddy reuses it instead of requesting a new certificate. `null` keeps certificate management with Caddy. |
| `reverseProxy.nginx.enable` | bool | `!reverseProxy.caddy.enable` | - | Whether to enable an nginx virtual host for Gradient. |
| `reverseProxy.nginx.manageTls` | bool | `true` | - | Whether to let nginx obtain and present the TLS certificate itself through the virtual host's `enableACME` and `forceSSL` settings. Disable it for upstream proxies (Traefik, Cloudflare, load balancers) that terminate TLS and forward plain HTTP to nginx. Keep `useTls` enabled in that case for `https://` URLs and `Secure` session cookies. Has no effect with `useTls` disabled. |

## `s3`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `s3.accessKeyId` | null or string | `null` | `GRADIENT_S3_ACCESS_KEY_ID` | AWS access key ID. Set it to `null` to use instance credentials or the environment. |
| `s3.bucket` | string | `""` | `GRADIENT_S3_BUCKET` | Name of the S3 bucket. The bucket must not have versioning, object lock or replication enabled. Object overwrites happen in place, and old versions stay in the bucket. Versioned buckets thus keep an unreclaimable copy per upload. |
| `s3.enable` | bool | `false` | - | Whether to enable storing NARs in S3. |
| `s3.endpoint` | null or string | `null` | `GRADIENT_S3_ENDPOINT` | Endpoint of an S3-compatible service such as MinIO or Cloudflare R2. Set it to `null` to use AWS. |
| `s3.maxRetries` | unsigned int | `3` | `GRADIENT_S3_MAX_RETRIES` | Retries of a failed S3 request. |
| `s3.prefix` | string | `""` | `GRADIENT_S3_PREFIX` | Key prefix inside the bucket, such as `gradient/`. Leave it empty to store at the bucket root. |
| `s3.readTimeoutSecs` | unsigned int | `60` | `GRADIENT_S3_READ_TIMEOUT_SECS` | Seconds an S3 response can stall before failing the request. Received chunks reset the timer, and large NARs keep streaming as long as they make progress. |
| `s3.region` | string | `"us-east-1"` | `GRADIENT_S3_REGION` | Region of the S3 bucket. |
| `s3.retryTimeoutSecs` | unsigned int | `250` | `GRADIENT_S3_RETRY_TIMEOUT_SECS` | Seconds after the first attempt past which no S3 retries start. Keep it above `(maxRetries + 1) * readTimeoutSecs` to still retry requests that fail on the read timeout. Keep it below 5 minutes as well, since retries reuse the original credentials. |
| `s3.secretAccessKeyFile` | null or path | `null` | `GRADIENT_S3_SECRET_ACCESS_KEY_FILE` | File containing the AWS secret access key. Set it to `null` to use instance credentials. |
| `s3.virtualHostedStyle` | bool | `false` | `GRADIENT_S3_VIRTUAL_HOSTED_STYLE` | Whether to address a custom `s3.endpoint` virtual-hosted style (`https://<bucket>.<endpoint>/key`) instead of path style (`https://<endpoint>/<bucket>/key`). MinIO, Garage and most self-hosted services need path style. It has no effect without a custom endpoint. |

## `scheduler`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `scheduler.clusterPrepareTimeoutSecs` | positive int | `30` | `GRADIENT_SCHEDULER_CLUSTER_PREPARE_TIMEOUT_SECS` | Seconds for every member of a cluster job attempt to accept its assignment. Attempts not accepted by all members in time abort, and their cluster jobs return to the queue. |
| `scheduler.clusterReserveAfterSecs` | positive int | `600` | `GRADIENT_SCHEDULER_CLUSTER_RESERVE_AFTER_SECS` | Seconds cluster jobs that can start wait for enough simultaneously idle workers before reserving a placement. Reserved workers receive no new single jobs until the cluster job's start or the reservation's expiry. |
| `scheduler.clusterReserveTimeoutSecs` | positive int | `1800` | `GRADIENT_SCHEDULER_CLUSTER_RESERVE_TIMEOUT_SECS` | Seconds to hold a cluster job reservation before releasing it and planning the cluster job again. |
| `scheduler.scoringPolicy` | one of `simple` `resource-aware` | `"resource-aware"` | `GRADIENT_SCHEDULER_SCORING_POLICY` | Policy ranking queued jobs for a requesting worker. Factors of `simple`: path availability, NAR size, dependency count, waiting time, builtins and fetch worker reservation. Factors of `resource-aware`: the same plus memory fit, worker saturation, CPU, disk and network affinity and `preferLocalBuild`. |

## `scim`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `scim.enable` | bool | `false` | `GRADIENT_SCIM_ENABLE` | Whether to enable SCIM provisioning. |
| `scim.hardDelete` | bool | `false` | `GRADIENT_SCIM_HARD_DELETE` | Whether to enable deleting users on SCIM `DELETE` instead of disabling them. |
| `scim.tokenFile` | path | - | `GRADIENT_SCIM_TOKEN_FILE` | File containing the SCIM bearer token. |

## `secrets`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `secrets.cryptFile` | path | - | `GRADIENT_SECRETS_CRYPT_FILE` | File containing the key used to encrypt secrets in the database. |
| `secrets.jwtFile` | path | - | `GRADIENT_SECRETS_JWT_FILE` | File containing the secret used to sign JWTs. |

## `sentry`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `sentry.dsn` | null or string | `null` | `GRADIENT_SENTRY_DSN` | Sentry DSN to use with `sentry.enable` set. Set it to `null` to send reports to the upstream Wavelens instance at `reports.wavelens.io`. |
| `sentry.enable` | bool | `false` | `GRADIENT_SENTRY_ENABLE` | Whether to enable error reporting to Sentry. |

## `ssh`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `ssh.enable` | bool | `false` | `GRADIENT_SSH_ENABLE` | Whether to enable the Nix daemon over SSH for `ssh-ng://` substituters, `nix copy` and `nixos-rebuild --build-host`. |
| `ssh.hostKeyFile` | null or path | `null` | `GRADIENT_SSH_HOST_KEY_FILE` | File containing the OpenSSH private host key. Set it to `null` to generate an ed25519 key in `baseDir` on first start. |
| `ssh.listenAddresses` | list of string | `[ "::" ]` | `GRADIENT_SSH_LISTEN_ADDRESSES` | IP addresses for the SSH server to listen on. IPv6 addresses also accept IPv4 clients. The default `::` thus means listening on every address of both families. |
| `ssh.openFirewall` | bool | `false` | - | Whether to enable the SSH port in the firewall. |
| `ssh.port` | port | `2222` | `GRADIENT_SSH_PORT` | Port for the SSH server to listen on. |

## `upload`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `upload.bytesBudget` | positive int | `8589934592` | `GRADIENT_UPLOAD_BYTES_BUDGET` | Total size in bytes of admitted uploads. Uploads that exceed the remaining budget wait. An upload larger than the whole budget can run alone once nothing else is in flight. |
| `upload.concurrency` | positive int | `8` | `GRADIENT_UPLOAD_CONCURRENCY` | Number of uploads over 1 MiB (NARs and eval cache segments) admitted at once across all workers and REST clients. Smaller uploads have a window of 128 of their own. Uploads hold their permit until the object is in storage. Other uploads wait for a permit in the meantime. |
| `upload.leaseIdleSecs` | positive int | `300` | `GRADIENT_UPLOAD_LEASE_IDLE_SECS` | Seconds granted worker uploads can go without data before they lose their permit and their workers receive a request to retry. |
| `upload.restWaitSecs` | positive int | `30` | `GRADIENT_UPLOAD_REST_WAIT_SECS` | Seconds NAR uploads to the cache upload endpoint wait for a permit before getting a 503 answer with `Retry-After`. |

## `worker`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.acceptedServerTokensFile` | null or path | `null` | `GRADIENT_WORKER_ACCEPTED_SERVER_TOKENS_FILE` | File of token hashes required from connecting servers, with `peer_id:hash` per line and `#` starting a comment. A hash is an argon2 PHC string or the lowercase SHA-256 hex digest of the token itself, for example from `printf %s "$TOKEN" \| sha256sum`. Peer ID `*` matches any peer. The worker reads this file only with `GRADIENT_WORKER_DISCOVERABLE`. Set it to `null` to accept every server; avoid that in a production setup. |
| `worker.baseDir` | path | `"/var/lib/gradient-worker"` | `GRADIENT_WORKER_BASE_DIR` | Directory holding the worker's state. |
| `worker.discoverable` | bool | `false` | `GRADIENT_WORKER_DISCOVERABLE` | Whether to enable incoming server connections on `/proto`. |
| `worker.domain` | string | `""` | - | Domain of the worker's reverse proxy virtual host. Only applies with a reverse proxy enabled. |
| `worker.enable` | bool | `false` | - | Whether to enable the Gradient worker. |
| `worker.endpoint` | null or string | `null` | `GRADIENT_WORKER_ENDPOINT` | Address at which other members of a cluster job connect to this worker, passed through verbatim in the cluster roster. `null` advertises none. |
| `worker.environmentVariables` | lazy attribute set of (null or bool or int or string or path) | `{ }` | - | Environment variables of the Gradient worker. The [configuration reference](https://wavelens.github.io/gradient/reference/configuration) lists every variable and its default. Set a variable to `null` to unset it. |
| `worker.gcrootsDir` | string | `"/nix/var/nix/gcroots/gradient"` | `GRADIENT_WORKER_GCROOTS_DIR` | Directory for the indirect GC roots pinning each running build's inputs and outputs against a concurrent `nix-collect-garbage`. An empty string disables pinning. |
| `worker.id` | null or string | `null` | `GRADIENT_WORKER_ID` | Worker UUID. `null` generates one on first start and stores it in `<worker.baseDir>/worker-id`. Set it when the UUID must be known in advance, for example to register the worker in `state.workers`. |
| `worker.idleTimeout` | null or (unsigned int) | `null` | `GRADIENT_WORKER_IDLE_TIMEOUT` | Seconds without an assigned job after which the worker drains and exits. `null` keeps it running. |
| `worker.listenAddr` | string | `"127.0.0.1"` | `GRADIENT_WORKER_LISTEN_ADDR` | IP address the worker listens on for incoming server connections. |
| `worker.maxLifetime` | null or (unsigned int) | `null` | `GRADIENT_WORKER_MAX_LIFETIME` | Seconds after start after which the worker drains, finishes its running jobs and exits. `null` keeps it running. |
| `worker.peersFile` | null or path | `null` | `GRADIENT_WORKER_PEERS_FILE` | File of peer tokens for challenge-response authentication with the server, with `peer_id:token` per line and `#` starting a comment. ``` <uuid>:<token> *:<token> ``` Use peer ID `*` to match any UUID in the server's challenge. Tokens are 48 byte random secrets, for example from `openssl rand -base64 48`. Register them via the API through `POST /api/v1/projects/{project}/workers`. Set it to `null` for open mode without authentication; avoid that in a production setup. |
| `worker.port` | port | `3100` | `GRADIENT_WORKER_PORT` | Port the worker listens on for incoming server connections. |
| `worker.serverUrl` | null or string | `null` | `GRADIENT_WORKER_SERVER_URL` | WebSocket URL of the Gradient server's `/proto` endpoint. `null` skips the outbound connection, leaving only incoming server connections through `worker.discoverable`. |
| `worker.useTls` | bool | `true` | - | Whether to enable TLS. |
| `worker.zone` | null or string | `null` | `GRADIENT_WORKER_ZONE` | Locality label advertised to the scheduler. A cluster job asking for one zone places every member on workers with the same label. `null` places the worker among the unlabelled workers. |

## `worker.build`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.build.cgroup` | null or string | `if worker.build.metrics then "/sys/fs/cgroup/system.slice/nix-daemon.service" else null` | `GRADIENT_WORKER_BUILD_CGROUP` | Cgroup directory of `nix-daemon.service`, the parent of the build cgroups. The worker reads each running build's peak memory from it and sends the values with its heartbeat. The scheduler reserves the larger of the predicted and the reported peak. The build cgroups need `worker.build.metrics` and Gradient's Nix fork on the daemon. `null` reports nothing. |
| `worker.build.maxConcurrent` | positive int | `32` | `GRADIENT_WORKER_BUILD_MAX_CONCURRENT` | Maximum simultaneous builds. |
| `worker.build.maxCores` | null or (positive int) | `null` | `GRADIENT_WORKER_BUILD_MAX_CORES` | CPU cores for a build, passed as `--cores`. `null` for every core. |
| `worker.build.metrics` | bool | `false` | - | Whether to record per-build peak memory, CPU time, disk I/O and out-of-memory kills. This turns on Nix's experimental `cgroups` feature and `use-cgroups` setting and delegates cgroup controllers to `nix-daemon.service`. The daemon reports the values in each build result. Peak memory, disk I/O and out-of-memory kills need Gradient's Nix fork on the daemon, and `nix.package` then defaults to its package. Wall-clock time is always recorded. |
| `worker.build.oomScoreAdjust` | null or integer between -1000 and 1000 (both inclusive) | `500` | - | Out-of-memory score adjustment for `nix-daemon.service`, inherited by every build process. A positive value makes the kernel kill a build process before the worker when the host runs out of memory, failing only that build. Evaluation subprocesses use 600 and die first. `null` leaves the daemon unchanged. |

## `worker.capabilities`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.capabilities.build` | bool | `true` | `GRADIENT_WORKER_CAPABILITIES_BUILD` | Whether to run Nix builds. |
| `worker.capabilities.eval` | bool | `true` | `GRADIENT_WORKER_CAPABILITIES_EVAL` | Whether to run Nix flake evaluations. |
| `worker.capabilities.exec` | bool | `false` | `GRADIENT_WORKER_CAPABILITIES_EXEC` | Whether this worker takes exec jobs. Exec jobs also need the server option `exec.enable`, the project switch `exec_enabled` and `enable_exec` on the worker registration. |
| `worker.capabilities.federate` | bool | `false` | `GRADIENT_WORKER_CAPABILITIES_FEDERATE` | Whether to forward work and NARs between workers and servers (requires `discoverable`). |
| `worker.capabilities.network` | bool | `true` | `GRADIENT_WORKER_CAPABILITIES_NETWORK` | Whether this worker may reach the internet. Cloning repositories, fetching flake inputs, substitutions, downloads and fixed-output builds run only on workers with it. |
| `worker.capabilities.secrets` | bool | `true` | `GRADIENT_WORKER_CAPABILITIES_SECRETS` | Whether the server may send project credentials such as SSH keys to this worker. |

## `worker.eval`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.eval.cache.dir` | null or string | `null` | `GRADIENT_WORKER_EVAL_CACHE_DIR` | Eval cache directory, exported to evaluation subprocesses as `NIX_CACHE_HOME`. `null` uses `<worker.baseDir>/eval-cache`. |
| `worker.eval.cache.share` | bool | `true` | `GRADIENT_WORKER_EVAL_CACHE_SHARE` | Whether to share the eval cache with other workers through the server. Evaluations pull the segments of other workers first and upload new rows as segments while they run. |
| `worker.eval.forkWorkers` | null or (positive int) | `null` | `GRADIENT_WORKER_EVAL_FORK_WORKERS` | Evaluation subprocesses in the pool, equal to the evaluation concurrency. `null` sizes the pool to the host's core count, capped at 16. |
| `worker.eval.maxConcurrent` | positive int | `1` | `GRADIENT_WORKER_EVAL_MAX_CONCURRENT` | Maximum simultaneous evaluations. |
| `worker.eval.maxMemory` | null or (positive int) | `null` | `GRADIENT_WORKER_EVAL_MAX_MEMORY` | Ceiling in MiB for the resident memory of all evaluation subprocesses together. Subprocesses leave the pool largest first while the total exceeds the ceiling. Conflicts with `worker.system.minFreeRamMb`. |
| `worker.eval.maxRss` | positive int | `2147483648` | `GRADIENT_WORKER_EVAL_MAX_RSS` | Resident memory in bytes for an idle evaluation subprocess. Subprocesses above the limit restart after the call and free the Nix heap. A call growing past the limit is fine, with `worker.system.minFreeRamMb` as the host's guard. Keep it above a typical evaluation's heap. |
| `worker.eval.metrics` | bool | `true` | `GRADIENT_WORKER_EVAL_METRICS` | Whether to collect per-evaluation Nix statistics (thunks, heap, peak memory, hotspots, flake graph). Disable it to skip that overhead. |

## `worker.exec`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.exec.allow.customImages` | bool | `false` | - | Whether exec jobs may boot their own guest image instead of `worker.exec.image`. |
| `worker.exec.allow.devices` | list of string | `[ ]` | - | Device names from `worker.exec.devices` exec jobs may request. An empty list allows none. |
| `worker.exec.allow.maxSilent` | null or (unsigned int) | `null` | - | Most seconds an exec job may go without output. The server lowers a larger request to this value. `null` sets no cap. |
| `worker.exec.allow.mounts` | list of string | `[ ]` | - | Mount names from `worker.exec.mounts` exec jobs may request. An empty list allows none. |
| `worker.exec.allow.network` | bool | `false` | - | Whether exec jobs may reach the network through `worker.exec.bridge`. |
| `worker.exec.allow.nodes` | unsigned int | `1` | - | Most nodes an exec job may span. |
| `worker.exec.allow.ports` | null or string | `null` | - | Lowest and highest guest port exec jobs may forward, as `low,high`. `null` allows ports 1024 to 65535. |
| `worker.exec.allow.retries` | unsigned int | `0` | - | Most retries an exec job may request. |
| `worker.exec.allow.ssh` | bool | `false` | - | Whether exec jobs may accept SSH sessions into their guest. |
| `worker.exec.allow.timeout` | null or (unsigned int) | `null` | - | Most seconds an exec job may run. The server lowers a larger request to this value. `null` sets no cap. |
| `worker.exec.bridge` | string | `"gradient-guests"` | `GRADIENT_WORKER_EXEC_BRIDGE` | Network bridge the guest tap devices join. |
| `worker.exec.cloudHypervisorBin` | string | `"cloud-hypervisor"` | `GRADIENT_WORKER_EXEC_CLOUD_HYPERVISOR_BIN` | Path to the `cloud-hypervisor` binary, resolved via `PATH` by default. |
| `worker.exec.defaults.maxSilent` | null or (unsigned int) | `null` | - | Seconds without output after which an exec job that leaves `maxSilent` unset times out. `null` sets no limit. |
| `worker.exec.defaults.mounts` | list of string | `[ ]` | - | Mounts of an exec job that leaves `mounts` unset. |
| `worker.exec.defaults.network` | bool | `false` | - | Whether an exec job that leaves `network` unset reaches the network. |
| `worker.exec.defaults.ssh` | bool | `false` | - | Whether an exec job that leaves `ssh` unset accepts SSH sessions. |
| `worker.exec.devices` | string | `"[]"` | `GRADIENT_WORKER_EXEC_DEVICES` | Host devices exec jobs can request, a JSON list in the environment variable. Each entry has a `name` and exactly one source: `vfio` lists PCI addresses bound to `vfio-pci`, one device each; `dev` names a device node; `usb` holds a `vendor:product` ID. |
| `worker.exec.image` | null or path | `null` | `GRADIENT_WORKER_EXEC_IMAGE` | Guest image from `gradient.lib.mkWorkerImage` that exec job guests boot. |
| `worker.exec.labels` | list of string | `[ ]` | `GRADIENT_WORKER_EXEC_LABELS` | Labels an exec job can ask for. The worker ID is always a label too. |
| `worker.exec.maxPreWarm` | unsigned int | `0` | `GRADIENT_WORKER_EXEC_MAX_PRE_WARM` | Most guests kept warm for upcoming exec jobs the server reports. The worker keeps the larger of `worker.exec.preWarm` and the reported count, capped at this value. `0` ignores the server count. |
| `worker.exec.mounts` | string | `"[]"` | `GRADIENT_WORKER_EXEC_MOUNTS` | Host directories exec jobs can mount at `/mnt/<name>`, a JSON list in the environment variable. Each entry has a `name`, a `path`, `readOnly` and `perJob`; a `perJob` mount gives every job a fresh directory below `path`. |
| `worker.exec.preWarm` | unsigned int | `0` | `GRADIENT_WORKER_EXEC_PRE_WARM` | Guests booted from `worker.exec.image` and paused, waiting for a job. |
| `worker.exec.stopGuestsWithWorker` | bool | `true` | `GRADIENT_WORKER_EXEC_STOP_GUESTS_WITH_WORKER` | Whether stopping or restarting the worker stops every guest. The server queues the exec jobs of stopped guests again without using a retry. `false` keeps guests running across a worker restart and the restarted worker reattaches to them. |
| `worker.exec.subnet` | string | `"10.233.0.0/16"` | `GRADIENT_WORKER_EXEC_SUBNET` | IPv4 subnet of `worker.exec.bridge`. The bridge takes the first host address, each guest the address of its vsock CID. |
| `worker.exec.virtiofsdBin` | string | `"virtiofsd"` | `GRADIENT_WORKER_EXEC_VIRTIOFSD_BIN` | Path to the `virtiofsd` binary, resolved via `PATH` by default. |

## `worker.log`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.log.burstBytesPerMin` | unsigned int | `8388608` | `GRADIENT_WORKER_LOG_BURST_BYTES_PER_MIN` | Build log bytes forwarded per build within any minute. The worker stops forwarding a build's log past this limit. The build itself continues. |
| `worker.log.fetchFromStore` | bool | `true` | `GRADIENT_WORKER_LOG_FETCH_FROM_STORE` | Whether to forward the stored build log of a derivation already built locally and therefore producing no new log. |
| `worker.log.level.build` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_WORKER_LOG_LEVEL_BUILD` | Log level of the builder. `null` uses `worker.log.level.default`. |
| `worker.log.level.default` | one of `trace` `debug` `info` `warn` `error` | `"info"` | `GRADIENT_WORKER_LOG_LEVEL_DEFAULT` | Default log level. |
| `worker.log.level.eval` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_WORKER_LOG_LEVEL_EVAL` | Log level of the evaluator. `null` uses `worker.log.level.default`. |
| `worker.log.level.proto` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_WORKER_LOG_LEVEL_PROTO` | Log level of the protocol layer. `null` uses `worker.log.level.default`. |
| `worker.log.sustainedBytesPerHour` | unsigned int | `67108864` | `GRADIENT_WORKER_LOG_SUSTAINED_BYTES_PER_HOUR` | Build log bytes forwarded per build within any hour. |
| `worker.log.traceDir` | null or path | `null` | `GRADIENT_WORKER_LOG_TRACE_DIR` | Directory receiving every closed stage span of the worker and its eval subprocesses as JSON lines, one file per process. `null` disables span tracing. |

## `worker.nar`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.nar.chunkBytes` | int from 4096 to 4194304 | `524288` | `GRADIENT_WORKER_NAR_CHUNK_BYTES` | Size in bytes of each outbound NAR upload chunk, from 4 KiB to 4 MiB. Smaller chunks can help slow or high-latency links like Tailscale over DERP. |
| `worker.nar.maxConcurrentUploads` | positive int | `16` | `GRADIENT_WORKER_NAR_MAX_CONCURRENT_UPLOADS` | Upload requests over 1 MiB open at once, waiting for a server grant or transferring. Half of the limit for any job, a window of 128 for smaller uploads. The server's upload budget decides how many run and bounds the worker's memory. |
| `worker.nar.partialTtlSecs` | unsigned int | `86400` | `GRADIENT_WORKER_NAR_PARTIAL_TTL_SECS` | Seconds after the last write before the worker deletes an unfinished NAR download under `<worker.baseDir>/nar-partial`. `0` for no cleanup. |

## `worker.nixDaemon`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.nixDaemon.maxConnections` | positive int | `worker.build.maxConcurrent * 9 + 16` | `GRADIENT_WORKER_NIX_DAEMON_MAX_CONNECTIONS` | Maximum connections to the local Nix daemon. Each build holds one for its whole run plus up to 8 for parallel NAR imports. The rest is headroom for path checks. |

## `worker.packages`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.packages.git` | package | derived | - | Git package available to the worker for cloning repositories. |
| `worker.packages.gradient` | package | derived | - | The gradient package to use. |
| `worker.packages.ssh` | package | derived | - | OpenSSH package used as `GIT_SSH_COMMAND` to fetch private flake inputs. |

## `worker.reverseProxy`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.reverseProxy.caddy.enable` | bool | `false` | - | Whether to enable a Caddy virtual host for the worker listener. |
| `worker.reverseProxy.caddy.extraConfig` | strings concatenated with "\n" | `""` | - | Additional lines appended to `services.caddy.virtualHosts.<name>.extraConfig` after the reverse proxy setup. |
| `worker.reverseProxy.caddy.useACMEHost` | null or string | `null` | - | Host of an existing ACME certificate, passed to `services.caddy.virtualHosts.<name>.useACMEHost`. Caddy reuses it instead of requesting a new certificate. `null` keeps certificate management with Caddy. |
| `worker.reverseProxy.nginx.enable` | bool | `false` | - | Whether to enable an nginx virtual host for the worker listener. |

## `worker.system`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.system.architectures` | list of string | derived | `GRADIENT_WORKER_SYSTEM_ARCHITECTURES` | Nix system types this worker builds for. |
| `worker.system.cpuCoreScore` | null or (positive int) | `null` | `GRADIENT_WORKER_SYSTEM_CPU_CORE_SCORE` | Single-core speed score advertised to the scheduler; higher is faster. `null` benchmarks the host at startup. |
| `worker.system.features` | list of string | `[ ]` | `GRADIENT_WORKER_SYSTEM_FEATURES` | Nix system features this worker advertises. An empty list uses the system features of the Nix store, including those that enabled experimental features imply, such as `recursive-nix`. |
| `worker.system.minFreeRamMb` | unsigned int | `0` | `GRADIENT_WORKER_SYSTEM_MIN_FREE_RAM_MB` | Free memory in MiB below which the worker kills the one evaluation subprocess large enough to restore the margin and reports that evaluation as failed instead of letting the host freeze. `0` uses 10% of total memory, clamped to 128 MiB to 1 GiB. `worker.eval.maxRss` still bounds steady-state memory. |
