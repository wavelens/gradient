# Configuration

Every option of the `services.gradient` NixOS module, generated from `nix/modules`. Each option will map onto a flag and an environment variable of the same name.

- `upload.bytesBudget` is `--upload-bytes-budget` and `GRADIENT_UPLOAD_BYTES_BUDGET`.
- `worker.build.maxConcurrent` is `--build-max-concurrent` and `GRADIENT_WORKER_BUILD_MAX_CONCURRENT`.
- Module-only options (`packages`, `reverseProxy`, `postgres`, ...) have no environment variable.
- Variables marked **(part)** are built from more than one option.

Declarative entities under `services.gradient.state` are in the [state reference](state.md).

## General

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `baseDir` | path | `"/var/lib/gradient"` | `GRADIENT_BASE_DIR` | Directory holding Gradient's state, NAR files and caches. |
| `domain` | string | - | - | Domain under which Gradient is reachable. |
| `enable` | bool | `false` | - | Whether to enable Gradient. |
| `listenAddr` | string | `"127.0.0.1"` | `GRADIENT_LISTEN_ADDR` | IP address the Gradient server is listening on. |
| `localWorker` | bool | `worker.enable` | - | Whether to provision credentials for a `worker` running on this host. These are a worker identity derived from the hostname, a token generated on first start, the matching peers file and a worker of the state-declared team `server`. New projects get the team's workers. |
| `port` | port | `3000` | `GRADIENT_PORT` | Port the Gradient server is listening on. |
| `retentionDays` | int | `90` | `GRADIENT_RETENTION_DAYS` | Days to keep job assignment records, finished deliveries, worker connection history, webhook and task action deliveries, expired sessions and CLI logins. The same limit will cover finished admin tasks, the audit log, per-build resource samples and finished cluster jobs. Removed resource samples are no longer feeding build predictions. A finished cluster job without remaining members will go on the next hourly pass. The cleanup will spare the newest finished admin task of each kind and active cluster jobs. An open worker connection is kept until the next connection of the same worker. `0` will keep every record forever. |
| `serveUrl` | string | derived | `GRADIENT_SERVE_URL` | Public URL under which clients are reaching Gradient. This option is needed for a URL other than `domain`, for example behind a port mapping. |
| `useQuic` | bool | `false` | `GRADIENT_USE_QUIC` | Whether to enable advertising HTTP/3 (QUIC) to clients. |
| `useTls` | bool | `true` | `GRADIENT_USE_TLS` | Whether to enable TLS. |

## `build`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `build.defaultMaxSilentSecs` | int | `3600` | `GRADIENT_BUILD_DEFAULT_MAX_SILENT_SECS` | Timeout in seconds without build output for derivations without `maxSilent`. |
| `build.defaultTimeoutSecs` | int | `14400` | `GRADIENT_BUILD_DEFAULT_TIMEOUT_SECS` | Build timeout in seconds for derivations without `timeout`. |
| `build.inputsUnavailableMaxLoops` | int | `3` | `GRADIENT_BUILD_INPUTS_UNAVAILABLE_MAX_LOOPS` | Times a build may retry after missing inputs before failing instead of retrying again. |
| `build.maxAttempts` | int | `3` | `GRADIENT_BUILD_MAX_ATTEMPTS` | Build or eval job attempts before a transient failure is permanent. |
| `build.retryBackoffSecs` | int | `30` | `GRADIENT_BUILD_RETRY_BACKOFF_SECS` | Seconds before retrying a transient build failure, doubled for every previous attempt. |
| `build.substituteMissEscalationThreshold` | int | `2` | `GRADIENT_BUILD_SUBSTITUTE_MISS_ESCALATION_THRESHOLD` | Free re-queues of a derivation available in a cache within one evaluation, before Gradient is building the derivation like any other. |

## `cache`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `cache.debugIndexIntervalSecs` | int | `300` | `GRADIENT_CACHE_DEBUG_INDEX_INTERVAL_SECS` | Seconds between DWARF build ID index backfill passes. |
| `cache.maxStorageGb` | int | `0` | `GRADIENT_CACHE_MAX_STORAGE_GB` | Instance-wide limit on cached NAR storage in GB. |
| `cache.signSweepIntervalSecs` | int | `3600` | `GRADIENT_CACHE_SIGN_SWEEP_INTERVAL_SECS` | Seconds between NAR signature backfill passes. |
| `cache.upstreamQueryConcurrency` | int | `32` | `GRADIENT_CACHE_UPSTREAM_QUERY_CONCURRENCY` | Maximum simultaneous narinfo requests to upstream caches across the server. |

## `database`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `database.cache.maxConnections` | int | `32` | `GRADIENT_DATABASE_CACHE_MAX_CONNECTIONS` | Maximum connections of the cache query pool. |
| `database.cache.minConnections` | int | `2` | `GRADIENT_DATABASE_CACHE_MIN_CONNECTIONS` | Minimum connections kept open in the cache query pool. |
| `database.maxConnections` | int | `16` | `GRADIENT_DATABASE_MAX_CONNECTIONS` | Maximum connections of the scheduler and worker pool. |
| `database.minConnections` | int | `2` | `GRADIENT_DATABASE_MIN_CONNECTIONS` | Minimum connections kept open in the scheduler and worker pool. |
| `database.url` | string | `"postgresql://gradient@localhost/gradient?host=/run/postgresql"` | - | PostgreSQL connection URL. |
| `database.urlFile` | path | derived | - | File containing the PostgreSQL connection URL. |
| `database.web.maxConnections` | int | `8` | `GRADIENT_DATABASE_WEB_MAX_CONNECTIONS` | Maximum connections of the HTTP API pool. |
| `database.web.minConnections` | int | `1` | `GRADIENT_DATABASE_WEB_MIN_CONNECTIONS` | Minimum connections kept open in the HTTP API pool. |

## `email`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `email.enable` | bool | `false` | `GRADIENT_EMAIL_ENABLE` | Whether to enable sending email. |
| `email.from.address` | string | - | `GRADIENT_EMAIL_FROM_ADDRESS` | Sender email address. |
| `email.from.name` | string | `"Gradient"` | `GRADIENT_EMAIL_FROM_NAME` | Sender display name. |
| `email.requireVerification` | bool | `false` | `GRADIENT_EMAIL_REQUIRE_VERIFICATION` | Whether to enable email verification for new accounts. |
| `email.smtp.host` | string | - | `GRADIENT_EMAIL_SMTP_HOST` | SMTP server host name. |
| `email.smtp.passwordFile` | path | - | - | File containing the SMTP password. |
| `email.smtp.port` | port | `587` | `GRADIENT_EMAIL_SMTP_PORT` | SMTP server port. |
| `email.smtp.useTls` | bool | `false` | `GRADIENT_EMAIL_SMTP_USE_TLS` | Whether to enable TLS for SMTP. |
| `email.smtp.username` | string | - | `GRADIENT_EMAIL_SMTP_USERNAME` | SMTP user name. |

## `eval`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `eval.cache.maxAgeDays` | int | `30` | `GRADIENT_EVAL_CACHE_MAX_AGE_DAYS` | Days after which an eval cache blob is evicted regardless of the size limit. |
| `eval.cache.maxTotalBytes` | int | `10737418240` | `GRADIENT_EVAL_CACHE_MAX_TOTAL_BYTES` | Total size in bytes of shared eval cache blobs. |
| `eval.cache.sweepIntervalSecs` | int | `3600` | `GRADIENT_EVAL_CACHE_SWEEP_INTERVAL_SECS` | Seconds between eval cache eviction passes. |
| `eval.maxKeep` | int | `30` | `GRADIENT_EVAL_MAX_KEEP` | Maximum number of evaluations kept per task. |

## `frontend`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `frontend.enable` | bool | `true` | - | Whether to enable the Gradient web frontend. |
| `frontend.url` | string | derived | `GRADIENT_FRONTEND_URL` | Public URL of the Gradient frontend, used for links in CI status reports. |

## `gc`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `gc.intervalSecs` | int | `3600` | `GRADIENT_GC_INTERVAL_SECS` | Seconds between garbage collection passes. |
| `gc.narTtlHours` | int | `336` | `GRADIENT_GC_NAR_TTL_HOURS` | Hours to keep a cached path outside the live closure of retained evaluations after its last fetch, or its upload if never fetched. |
| `gc.narUploadGraceHours` | int | `24` | `GRADIENT_GC_NAR_UPLOAD_GRACE_HOURS` | Hours before the deletion of an unreferenced NAR object, covering the window between its upload and the commit of its database rows. |
| `gc.orphanDerivationHours` | int | `24` | `GRADIENT_GC_ORPHAN_DERIVATION_HOURS` | Hours before the deletion of a derivation outside the build closure of every retained evaluation. |
| `gc.wedgedEvalHours` | int | `24` | `GRADIENT_GC_WEDGED_EVAL_HOURS` | Hours an evaluation may stay in one phase before counting as stuck. A stuck evaluation is no longer blocking evaluation garbage collection. |
| `gc.deepIntervalSecs` | int | `3600` | `GRADIENT_GC_DEEP_INTERVAL_SECS` | Seconds from the end of one background [deep garbage collection](../contributors/internals/nar-storage.md#deep-gc) round to the start of the next. A value of `0` is running a round only on request. |
| `gc.deepPaceMs` | int | `1000` | `GRADIENT_GC_DEEP_PACE_MS` | Milliseconds between two units of a [storage migration](../contributors/internals/nar-storage.md#storage-migrations) or a background deep garbage collection round. A requested round is running its units without a pause. |

## `githubApp`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `githubApp.enable` | bool | `false` | - | Whether to enable the GitHub App integration for webhooks and CI status reports. |
| `githubApp.id` | int | - | `GRADIENT_GITHUB_APP_ID` | GitHub App ID, shown on the App's settings page. |
| `githubApp.privateKeyFile` | path | - | - | File containing the GitHub App's RS256 private key in PEM format. |
| `githubApp.webhookSecretFile` | path | - | - | File containing the secret for verifying GitHub App webhook payloads. |

## `gradientCi`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `gradientCi.enable` | bool | `true` | `GRADIENT_GRADIENT_CI_ENABLE` | Whether to enable the Gradient.CI Servers offer on the workers pages. Existing connections keep working when the offer is off. See [Connect Gradient.CI Servers](../guides/gradient-ci-servers.md). |
| `gradientCi.url` | string | `"https://servers.gradient.ci"` | `GRADIENT_GRADIENT_CI_URL` | Address of Gradient.CI Servers. "Connect" is opening `<url>/connect`, and a new connection is dialing the host of this address at `/proto`. |

## `http`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `http.localIps` | list of string | `[ "192.168.0.0/16" "172.16.0.0/12" "100.64.0.0/10" "10.0.0.0/8" "fc00::/7" ]` | `GRADIENT_HTTP_LOCAL_IPS` | CIDR ranges whose clients are receiving a cache's `local_priority`, if set and non-zero. |
| `http.maxRequestSize` | int | `2097152` | `GRADIENT_HTTP_MAX_REQUEST_SIZE` | Maximum HTTP request body size in bytes for most endpoints, keeping an unbounded body from exhausting server memory. Build request blob uploads are using a fixed 20 MiB cap. |
| `http.maxSourceUploadSize` | int | `536870912` | `GRADIENT_HTTP_MAX_SOURCE_UPLOAD_SIZE` | Maximum size in bytes of a source upload to `POST /build-requests/source` (as sent by `gradient build`) and of a chunked manifest in total. |
| `http.trustedProxies` | list of string | `[ "127.0.0.1/8" "::1/128" ]` | `GRADIENT_HTTP_TRUSTED_PROXIES` | CIDR ranges of peers allowed to set `X-Forwarded-For`. |

## `log`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `log.chunkBytes` | int | `262144` | `GRADIENT_LOG_CHUNK_BYTES` | Target uncompressed size in bytes of a stored build log chunk. |
| `log.level.cache` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_LOG_LEVEL_CACHE` | Log level of the cache. |
| `log.level.default` | one of `trace` `debug` `info` `warn` `error` | `"info"` | `GRADIENT_LOG_LEVEL_DEFAULT` | Default log level. |
| `log.level.proto` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_LOG_LEVEL_PROTO` | Log level of the protocol layer. |
| `log.level.scheduler` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_LOG_LEVEL_SCHEDULER` | Log level of the scheduler. |
| `log.level.web` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_LOG_LEVEL_WEB` | Log level of the web API. |
| `log.traceDir` | null or string | `null` | `GRADIENT_LOG_TRACE_DIR` | Directory receiving every closed stage span of the server as JSON lines, one file per process. `null` is disabling span tracing. |

## `metrics`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `metrics.cacheFlushIntervalSecs` | int | `10` | `GRADIENT_METRICS_CACHE_FLUSH_INTERVAL_SECS` | Seconds between flushes of cache traffic counters to the database. |
| `metrics.graphConsistencyIntervalSecs` | int | `300` | `GRADIENT_METRICS_GRAPH_CONSISTENCY_INTERVAL_SECS` | Seconds between build graph consistency checks. |
| `metrics.instanceIntervalSecs` | int | `30` | `GRADIENT_METRICS_INSTANCE_INTERVAL_SECS` | Seconds between updates of the instance-wide metric window. |
| `metrics.labelTopn` | int | `20` | `GRADIENT_METRICS_LABEL_TOPN` | Maximum distinct label values per rollup dimension, by activity. |
| `metrics.otlp.endpoint` | null or string | `null` | `GRADIENT_METRICS_OTLP_ENDPOINT` | OTLP collector endpoint to push metrics to. |
| `metrics.otlp.pushIntervalSecs` | int | `30` | `GRADIENT_METRICS_OTLP_PUSH_INTERVAL_SECS` | Seconds between OTLP metric pushes. |
| `metrics.retention.rawDays` | int | `14` | `GRADIENT_METRICS_RETENTION_RAW_DAYS` | Days to keep raw phase and worker samples and the per-minute cache and upstream traffic counters. `0` is keeping them forever. |
| `metrics.retention.rollupDays` | int | `400` | `GRADIENT_METRICS_RETENTION_ROLLUP_DAYS` | Days to keep minute and hour rollups. Day and week rollups are staying forever. |
| `metrics.rollupIntervalSecs` | int | `60` | `GRADIENT_METRICS_ROLLUP_INTERVAL_SECS` | Seconds between metric rollup passes. |
| `metrics.tokenFile` | null or path | `null` | - | File containing the bearer token required to scrape `GET /metrics`. |
| `metrics.workerSampleIntervalSecs` | int | `15` | `GRADIENT_METRICS_WORKER_SAMPLE_INTERVAL_SECS` | Seconds between worker metric samples. |

## `nar`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `nar.hotCacheBytes` | int | `536870912` | `GRADIENT_NAR_HOT_CACHE_BYTES` | Capacity in bytes of the in-memory NAR cache. |
| `nar.maxConcurrentDownloads` | int | `16` | `GRADIENT_NAR_MAX_CONCURRENT_DOWNLOADS` | NAR downloads from storage that may run at once across all connections, keeping the storage near its best total throughput. |
| `nar.maxConcurrentServes` | int | `8` | `GRADIENT_NAR_MAX_CONCURRENT_SERVES` | NAR serving tasks that may run at once per worker connection, bounding memory and storage fan-out for large batches. |
| `nar.maxUploadSize` | int | `536870912` | `GRADIENT_NAR_MAX_UPLOAD_SIZE` | Maximum size in bytes of a NAR uploaded to the cache upload endpoint. |
| `nar.partialTtlSecs` | int | `86400` | `GRADIENT_NAR_PARTIAL_TTL_SECS` | Seconds since the last write of an unfinished upload staged under `<baseDir>`, after which the next [deep GC](../contributors/internals/nar-storage.md#deep-gc) is removing the upload. `0` is keeping every unfinished upload. |
| `nar.sendChunkTimeoutSecs` | int | `30` | `GRADIENT_NAR_SEND_CHUNK_TIMEOUT_SECS` | Seconds an outbound `NarPush` chunk may wait for the WebSocket to drain before an abort of the transfer with `NarAbort`. |
| `nar.smallBytes` | int | `1048576` | `GRADIENT_NAR_SMALL_BYTES` | Size in bytes up to which the server is serving a NAR download itself instead of a presigned S3 URL. NARs up to this size also stay in the in-memory cache. Uploads do not depend on this value. |
| `nar.storageOpenTimeoutSecs` | int | `60` | `GRADIENT_NAR_STORAGE_OPEN_TIMEOUT_SECS` | Seconds to wait for a NAR object stream from storage (for example an S3 GET) before answering the worker with `NarAbort`. The worker is retrying after a `NarAbort`. |
| `nar.verifyDigest` | bool | `false` | `GRADIENT_NAR_VERIFY_DIGEST` | Whether to download NARs committed through presigned S3 uploads and verify their hash, catching same-length corruption at the cost of a full object read. |

## `oidc`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `oidc.clientId` | string | - | `GRADIENT_OIDC_CLIENT_ID` | OIDC client ID. |
| `oidc.clientSecretFile` | path | - | - | File containing the OIDC client secret. |
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
| `permissions.createCache` | one of `none` `superusers` `everyone` | `"everyone"` | `GRADIENT_PERMISSIONS_CREATE_CACHE` | Who may create caches through the API: `none` (only the declarative state), `superusers`, or `everyone` (any authenticated user). |
| `permissions.createProject` | one of `none` `superusers` `everyone` | `"everyone"` | `GRADIENT_PERMISSIONS_CREATE_PROJECT` | Who may create projects through the API: `none` (only the declarative state), `superusers`, or `everyone` (any authenticated user). |
| `permissions.createTeam` | one of `none` `superusers` `everyone` | `"everyone"` | `GRADIENT_PERMISSIONS_CREATE_TEAM` | Who may create teams through the API: `none` (only the declarative state), `superusers`, or `everyone` (any authenticated user). |

## `postgres`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `postgres.effectiveCacheSize` | null or string | `null` | - | `effective_cache_size` of the cluster set up by `postgres.enable`, typically three quarters of the host's RAM. |
| `postgres.enable` | bool | `false` | - | Whether to enable a local PostgreSQL database for Gradient. |
| `postgres.maintenanceWorkMem` | null or string | `null` | - | `maintenance_work_mem` of the cluster set up by `postgres.enable`, used by index builds and autovacuum. |
| `postgres.sharedBuffers` | null or string | `"512MB"` | - | `shared_buffers` of the cluster set up by `postgres.enable`. |
| `postgres.workMem` | null or string | `null` | - | `work_mem` of the cluster set up by `postgres.enable`. |

## `proto`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `proto.anonymousCache.enable` | bool | `true` | `GRADIENT_PROTO_ANONYMOUS_CACHE_ENABLE` | Whether unauthenticated clients may use `GET /cache/{cache}/proto` for public caches. |
| `proto.anonymousCache.maxConnectionsPerIp` | int | `32` | `GRADIENT_PROTO_ANONYMOUS_CACHE_MAX_CONNECTIONS_PER_IP` | Maximum simultaneous anonymous `/cache/proto` connections per client IP. |
| `proto.discoverable` | bool | `true` | `GRADIENT_PROTO_DISCOVERABLE` | Whether to enable incoming worker and federation connections on `/proto`. |
| `proto.federate` | bool | `false` | `GRADIENT_PROTO_FEDERATE` | Whether to enable federation with other Gradient servers over `/proto`. |
| `proto.maxConnections` | int | `256` | `GRADIENT_PROTO_MAX_CONNECTIONS` | Maximum simultaneous `/proto` WebSocket connections. |
| `proto.public` | bool | `false` | - | Whether to enable exposing `/proto` through the reverse proxy for remote workers and federation. |
| `proto.workerHeartbeatTimeoutSecs` | int | `120` | `GRADIENT_PROTO_WORKER_HEARTBEAT_TIMEOUT_SECS` | Seconds a connected worker may stay silent before the server is declaring the worker dead and re-queuing its jobs. |

## `pullRequests`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `pullRequests.commitEmail` | null or string | `null` | `GRADIENT_PULL_REQUESTS_COMMIT_EMAIL` | Git author and committer email for commits pushed by the `open_pr` action. |
| `pullRequests.commitName` | null or string | `null` | `GRADIENT_PULL_REQUESTS_COMMIT_NAME` | Git author and committer name for commits pushed by the `open_pr` action. |

## `registration`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `registration.enable` | bool | `true` | `GRADIENT_REGISTRATION_ENABLE` | Whether to enable self-service user registration. |

## `reverseProxy`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `reverseProxy.caddy.enable` | bool | `false` | - | Whether to enable a Caddy virtual host for Gradient. |
| `reverseProxy.caddy.extraConfig` | strings concatenated with "\n" | `""` | - | Additional lines appended to `services.caddy.virtualHosts.<name>.extraConfig` after the reverse proxy setup. |
| `reverseProxy.caddy.useACMEHost` | null or string | `null` | - | Host of an existing ACME certificate to use, passed to `services.caddy.virtualHosts.<name>.useACMEHost`. |
| `reverseProxy.nginx.enable` | bool | `!reverseProxy.caddy.enable` | - | Whether to enable an nginx virtual host for Gradient. |
| `reverseProxy.nginx.manageTls` | bool | `true` | - | Whether nginx is obtaining and serving the TLS certificate itself, by setting the virtual host's `enableACME` and `forceSSL`. |

## `s3`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `s3.accessKeyId` | null or string | `null` | `GRADIENT_S3_ACCESS_KEY_ID` | AWS access key ID. |
| `s3.bucket` | string | `""` | `GRADIENT_S3_BUCKET` | Name of the S3 bucket. |
| `s3.enable` | bool | `false` | - | Whether to enable storing NARs in S3. |
| `s3.endpoint` | null or string | `null` | `GRADIENT_S3_ENDPOINT` | Endpoint of an S3-compatible service such as MinIO or Cloudflare R2. |
| `s3.maxRetries` | int | `3` | `GRADIENT_S3_MAX_RETRIES` | Retries of a failed S3 request. |
| `s3.prefix` | string | `""` | `GRADIENT_S3_PREFIX` | Key prefix inside the bucket, such as `gradient/`. |
| `s3.readTimeoutSecs` | int | `60` | `GRADIENT_S3_READ_TIMEOUT_SECS` | Seconds an S3 response may stall before the request is failing. |
| `s3.region` | string | `"us-east-1"` | `GRADIENT_S3_REGION` | Region of the S3 bucket. |
| `s3.retryTimeoutSecs` | int | `250` | `GRADIENT_S3_RETRY_TIMEOUT_SECS` | Seconds after the first attempt past which no S3 retry is starting. |
| `s3.secretAccessKeyFile` | null or path | `null` | - | File containing the AWS secret access key. |
| `s3.virtualHostedStyle` | bool | `false` | `GRADIENT_S3_VIRTUAL_HOSTED_STYLE` | Whether to address a custom `s3.endpoint` virtual-hosted style (`https://<bucket>.<endpoint>/key`) instead of path style (`https://<endpoint>/<bucket>/key`). |

## `scheduler`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `scheduler.clusterPrepareTimeoutSecs` | int | `30` | `GRADIENT_SCHEDULER_CLUSTER_PREPARE_TIMEOUT_SECS` | Seconds for every member of a cluster job attempt to accept its assignment. |
| `scheduler.clusterReserveAfterSecs` | int | `600` | `GRADIENT_SCHEDULER_CLUSTER_RESERVE_AFTER_SECS` | Seconds a cluster job that can start is waiting for enough simultaneously idle workers before reserving a placement. |
| `scheduler.clusterReserveTimeoutSecs` | int | `1800` | `GRADIENT_SCHEDULER_CLUSTER_RESERVE_TIMEOUT_SECS` | Seconds to hold a cluster job reservation before the scheduler is releasing the reservation and planning the cluster job again. |
| `scheduler.scoringPolicy` | one of `simple` `resource-aware` | `"resource-aware"` | `GRADIENT_SCHEDULER_SCORING_POLICY` | Policy ranking queued jobs for a requesting worker. |

## `scim`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `scim.enable` | bool | `false` | `GRADIENT_SCIM_ENABLE` | Whether to enable SCIM provisioning. |
| `scim.hardDelete` | bool | `false` | `GRADIENT_SCIM_HARD_DELETE` | Whether to enable deleting users on SCIM `DELETE` instead of disabling them. |
| `scim.tokenFile` | path | - | - | File containing the SCIM bearer token. |

## `secrets`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `secrets.cryptFile` | path | - | - | File containing the key used to encrypt secrets in the database. |
| `secrets.jwtFile` | path | - | - | File containing the secret used to sign JWTs. |

## `sentry`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `sentry.dsn` | null or string | `null` | `GRADIENT_SENTRY_DSN` | Sentry DSN used when `sentry.enable` is set. |
| `sentry.enable` | bool | `false` | `GRADIENT_SENTRY_ENABLE` | Whether to enable error reporting to Sentry. |

## `ssh`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `ssh.enable` | bool | `false` | `GRADIENT_SSH_ENABLE` | Whether to enable the Nix daemon over SSH for `ssh-ng://` substituters, `nix copy` and `nixos-rebuild --build-host`. |
| `ssh.hostKeyFile` | null or path | `null` | `GRADIENT_SSH_HOST_KEY_FILE` | File containing the OpenSSH private host key. If unset, an ed25519 key is generated in `baseDir` on first start. |
| `ssh.listenAddress` | string | `"::"` | `GRADIENT_SSH_LISTEN_ADDRESS` | IP address the SSH server is listening on. An IPv6 address is also accepting IPv4 clients. |
| `ssh.openFirewall` | bool | `false` | - | Whether to enable the SSH port in the firewall. |
| `ssh.port` | port | `2222` | `GRADIENT_SSH_PORT` | Port the SSH server is listening on. |

## `upload`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `upload.bytesBudget` | int | `8589934592` | `GRADIENT_UPLOAD_BYTES_BUDGET` | Total size in bytes of admitted uploads. |
| `upload.concurrency` | int | `8` | `GRADIENT_UPLOAD_CONCURRENCY` | Uploads over 1 MiB (NARs and eval cache blobs) admitted at once across all workers and REST clients. Smaller uploads have a window of 128 of their own. An upload is holding its permit until the object is in storage. |
| `upload.leaseIdleSecs` | int | `300` | `GRADIENT_UPLOAD_LEASE_IDLE_SECS` | Seconds a granted worker upload may go without data before the server is reclaiming its permit and telling the worker to retry. |
| `upload.restWaitSecs` | int | `30` | `GRADIENT_UPLOAD_REST_WAIT_SECS` | Seconds a NAR upload to the cache upload endpoint is waiting for a permit before the server is answering with 503 and `Retry-After`. |

## `worker`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.acceptedServerTokensFile` | null or path | `null` | - | File of token hashes a dialing server must present, one `peer_id:hash` per line. A hash is an argon2 PHC string or the SHA-256 hex digest from `printf %s "$TOKEN" \| sha256sum`. `null` is accepting every server. |
| `worker.baseDir` | path | `"/var/lib/gradient-worker"` | `GRADIENT_WORKER_BASE_DIR` | Directory holding the worker's state. |
| `worker.discoverable` | bool | `false` | `GRADIENT_WORKER_DISCOVERABLE` | Whether to enable incoming server connections on `/proto`. |
| `worker.domain` | string | `""` | - | Domain of the worker's reverse proxy virtual host. |
| `worker.drainTimeoutSecs` | int | `60` | `GRADIENT_WORKER_DRAIN_TIMEOUT_SECS` | Seconds a stop is waiting for running jobs. |
| `worker.enable` | bool | `false` | - | Whether to enable the Gradient worker. |
| `worker.endpoint` | null or string | `null` | `GRADIENT_WORKER_ENDPOINT` | Address at which other members of a cluster job are reaching this worker, passed through verbatim in the cluster roster. |
| `worker.gcrootsDir` | string | `"/nix/var/nix/gcroots/gradient"` | `GRADIENT_WORKER_GCROOTS_DIR` | Directory for the indirect GC roots pinning each running build's inputs and outputs against a concurrent `nix-collect-garbage`. |
| `worker.id` | null or string | `null` | `GRADIENT_WORKER_ID` | Worker UUID. |
| `worker.listenAddr` | string | `"127.0.0.1"` | `GRADIENT_WORKER_LISTEN_ADDR` | IP address the worker is listening on for incoming server connections. |
| `worker.peersFile` | null or path | `null` | - | File of peer tokens for challenge-response authentication with the server, one `peer_id:token` per line. |
| `worker.port` | port | `3100` | `GRADIENT_WORKER_PORT` | Port the worker is listening on for incoming server connections. |
| `worker.serverUrl` | null or string | `null` | `GRADIENT_WORKER_SERVER_URL` | WebSocket URL of the Gradient server's `/proto` endpoint. |
| `worker.useTls` | bool | `true` | - | Whether to enable TLS. |
| `worker.zone` | null or string | `null` | `GRADIENT_WORKER_ZONE` | Locality label advertised to the scheduler. A cluster job asking for one zone is placing every member on workers with the same label. `null` is putting the worker in the zone of all unlabelled workers. |

## `worker.build`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.build.maxConcurrent` | int | `32` | `GRADIENT_WORKER_BUILD_MAX_CONCURRENT` | Maximum simultaneous builds. |
| `worker.build.maxCores` | null or (int) | `null` | `GRADIENT_WORKER_BUILD_MAX_CORES` | CPU cores a single build may use, passed as `--cores`. |
| `worker.build.metrics` | bool | `false` | - | Whether to record per-build peak memory, CPU time, disk I/O and out-of-memory kills. |

## `worker.capabilities`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.capabilities.build` | bool | `true` | `GRADIENT_WORKER_CAPABILITIES_BUILD` | Whether to enable Nix builds. |
| `worker.capabilities.eval` | bool | `true` | `GRADIENT_WORKER_CAPABILITIES_EVAL` | Whether to enable Nix flake evaluations. |
| `worker.capabilities.federate` | bool | `false` | `GRADIENT_WORKER_CAPABILITIES_FEDERATE` | Whether to enable forwarding work and NARs between workers and servers (requiring `discoverable`). |
| `worker.capabilities.fetch` | bool | `true` | `GRADIENT_WORKER_CAPABILITIES_FETCH` | Whether to enable prefetching flake inputs and sources. |

## `worker.eval`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.eval.cache.dir` | null or string | `null` | `GRADIENT_WORKER_EVAL_CACHE_DIR` | Eval cache directory, exported to evaluation subprocesses as `NIX_CACHE_HOME`. |
| `worker.eval.cache.share` | bool | `true` | `GRADIENT_WORKER_EVAL_CACHE_SHARE` | Whether to share eval cache blobs with other workers through the server. |
| `worker.eval.forkWorkers` | null or (int) | `null` | `GRADIENT_WORKER_EVAL_FORK_WORKERS` | Evaluation subprocesses in the pool, equal to the evaluation concurrency. `null` is sizing the pool to the host's core count, capped at 16. The pool is shrinking further until its size times `worker.eval.maxRss` is fitting in 75% of the host's RAM. |
| `worker.eval.maxConcurrent` | int | `1` | `GRADIENT_WORKER_EVAL_MAX_CONCURRENT` | Maximum simultaneous evaluations. |
| `worker.eval.maxRss` | int | `2147483648` | `GRADIENT_WORKER_EVAL_MAX_RSS` | Memory in bytes above which the worker is recycling an evaluation subprocess after its current call. The limit is not hard. A subprocess may exceed the limit during a call. |
| `worker.eval.metrics` | bool | `true` | `GRADIENT_WORKER_EVAL_METRICS` | Whether to collect per-evaluation Nix statistics (thunks, heap, peak memory, hotspots, flake graph). |

## `worker.log`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.log.burstBytesPerMin` | int | `8388608` | `GRADIENT_WORKER_LOG_BURST_BYTES_PER_MIN` | Build log bytes forwarded per build within any minute. |
| `worker.log.fetchFromStore` | bool | `true` | `GRADIENT_WORKER_LOG_FETCH_FROM_STORE` | Whether to forward the stored build log of a derivation already built locally and therefore producing no new log. |
| `worker.log.level.build` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_WORKER_LOG_LEVEL_BUILD` | Log level of the builder. |
| `worker.log.level.default` | one of `trace` `debug` `info` `warn` `error` | `"info"` | `GRADIENT_WORKER_LOG_LEVEL_DEFAULT` | Default log level. |
| `worker.log.level.eval` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_WORKER_LOG_LEVEL_EVAL` | Log level of the evaluator. |
| `worker.log.level.proto` | null or one of `trace` `debug` `info` `warn` `error` | `null` | `GRADIENT_WORKER_LOG_LEVEL_PROTO` | Log level of the protocol layer. |
| `worker.log.sustainedBytesPerHour` | int | `67108864` | `GRADIENT_WORKER_LOG_SUSTAINED_BYTES_PER_HOUR` | Build log bytes forwarded per build within any hour. |
| `worker.log.traceDir` | null or string | `null` | `GRADIENT_WORKER_LOG_TRACE_DIR` | Directory receiving every closed stage span of the worker and its eval subprocesses as JSON lines, one file per process. `null` is disabling span tracing. |

## `worker.nar`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.nar.maxConcurrentUploads` | int | `16` | `GRADIENT_WORKER_NAR_MAX_CONCURRENT_UPLOADS` | Upload requests over 1 MiB the worker is keeping open at once, waiting for a server grant or transferring. One job is holding at most half. Smaller uploads have a window of 128 of their own. |
| `worker.nar.partialTtlSecs` | int | `86400` | `GRADIENT_WORKER_NAR_PARTIAL_TTL_SECS` | Seconds after its last write before the deletion of an unfinished NAR download under `<worker.baseDir>/nar-partial`. |

## `worker.nixDaemon`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.nixDaemon.maxConnections` | int | `worker.build.maxConcurrent * 9 + 16` | `GRADIENT_WORKER_NIX_DAEMON_MAX_CONNECTIONS` | Maximum connections to the local Nix daemon. |

## `worker.packages`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.packages.git` | package | `config.programs.git.package` | - | Git package available to the worker for cloning repositories. |
| `worker.packages.gradient` | package | derived | - | The gradient package to use. |
| `worker.packages.nix` | package | derived | `GRADIENT_WORKER_NIX_BIN` (part) | Nix package whose `nix` the worker is running to detect its system features. |
| `worker.packages.ssh` | package | `config.programs.ssh.package` | `GRADIENT_WORKER_SSH_BIN` (part) | OpenSSH package used as `GIT_SSH_COMMAND` to fetch private flake inputs. |

## `worker.reverseProxy`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.reverseProxy.caddy.enable` | bool | `false` | - | Whether to enable a Caddy virtual host for the worker listener. |
| `worker.reverseProxy.caddy.extraConfig` | strings concatenated with "\n" | `""` | - | Additional lines appended to `services.caddy.virtualHosts.<name>.extraConfig` after the reverse proxy setup. |
| `worker.reverseProxy.caddy.useACMEHost` | null or string | `null` | - | Host of an existing ACME certificate to use, passed to `services.caddy.virtualHosts.<name>.useACMEHost`. |
| `worker.reverseProxy.nginx.enable` | bool | `false` | - | Whether to enable an nginx virtual host for the worker listener. |

## `worker.system`

| Option | Type | Default | Env | Description |
|---|---|---|---|---|
| `worker.system.architectures` | list of string | derived | `GRADIENT_WORKER_SYSTEM_ARCHITECTURES` | Nix system types this worker is building for. |
| `worker.system.cpuCoreScore` | null or (int) | `null` | `GRADIENT_WORKER_SYSTEM_CPU_CORE_SCORE` | Single-core speed score advertised to the scheduler, higher is faster. |
| `worker.system.features` | list of string | `[ ]` | `GRADIENT_WORKER_SYSTEM_FEATURES` | Nix system features this worker is advertising. |
| `worker.system.minFreeRamMb` | int | `0` | `GRADIENT_WORKER_SYSTEM_MIN_FREE_RAM_MB` | Free memory in MiB below which the worker is killing the one evaluation subprocess large enough to restore the margin. The worker is reporting that evaluation as failed instead of letting the host freeze. |

## Build Failures

| Status | Retried | Cause |
|---|---|---|
| `FailedPermanent` | No | The builder exited non-zero |
| `FailedTransient` | Yes, `build.maxAttempts` times with a doubling `build.retryBackoffSecs` | Out of memory, full disk, network or substitution failure, builder crash |
| `FailedTimeout` | No | `build.defaultTimeoutSecs` or `build.defaultMaxSilentSecs` exceeded |

The derivation attributes `timeout`, `maxSilent` and `preferLocalBuild` will override the server defaults. `meta.*` attributes are never reaching the `.drv` and have no effect here.

## Local Worker

`localWorker` will register the worker of the same host without manual steps if `worker.enable` is set.

- A stable worker ID derived from the host name.
- A token generated on first start in `/var/lib/gradient-worker/local-token`. A deletion of the file and a restart of both services will rotate the token.
- A worker of the state-declared team `server`, set to grant every new project its workers. Projects created before the team existed get the worker once they grant the `server` team.

The worker will wait in reconnect backoff (at most 60 s) until a project with a cache subscription is available.

## Postgres Sizing

`postgres.enable` will set host-independent defaults, each a `mkDefault`.

| Setting | Default | Reason |
|---|---|---|
| `random_page_cost` | `1.1` | SSDs. Keeping the planner on index-only scans of the graph tables |
| `max_connections` | `200` | Covering the three server pools (56 connections by default) plus autovacuum and `psql` |
| `max_locks_per_transaction` | `1024` | Graph writes are locking every shared build they touch. The server is warning below `256` |

The RAM-dependent settings are module options.

| Option | Rule of thumb |
|---|---|
| `postgres.sharedBuffers` | A quarter of the RAM, e.g. `"4GB"` on 16 GB |
| `postgres.effectiveCacheSize` | Three quarters of the RAM, e.g. `"12GB"` on 16 GB |
| `postgres.workMem` | `"32MB"` for the default pool sizes |
| `postgres.maintenanceWorkMem` | Up to `"1GB"` with RAM to spare |

An external database (`database.urlFile`) must have the same seven values, set by hand.

## Prometheus and OpenTelemetry

Setup, metric names and alert examples are in [Monitor Gradient](../guides/monitoring.md).

## Hashing

Gradient will hash NARs and cache files with SHA-256. Nix clients need no experimental feature. `blake3:` hashes from older uploads are still resolving.
