# Configuration

Gradient is configured through its **NixOS modules**: `services.gradient` for the server and `services.gradient.worker` for workers. Every module option maps onto a command-line flag and an environment variable of the same name:

- Server: option `nar.commitConcurrency` is flag `--nar-commit-concurrency` and env `GRADIENT_NAR_COMMIT_CONCURRENCY`.
- Worker: option `worker.build.maxConcurrent` is flag `--build-max-concurrent` and env `GRADIENT_WORKER_BUILD_MAX_CONCURRENT`.
- An `enable` option maps to `..._ENABLE` / `--...-enable`.
- Module-only options (`packages`, `reverseProxy`, `postgres`, `domain`, ...) have no env.

## Minimal Setup

```nix
services.gradient = {
  enable = true;
  domain = "gradient.example.com";
  postgres.enable = true;
  reverseProxy.nginx.enable = true;
  secrets = {
    jwtFile = "/run/secrets/gradient-jwt";
    cryptFile = "/run/secrets/gradient-crypt";
  };
};
```

`postgres.enable` creates a local PostgreSQL database and user. `reverseProxy` adds a virtual host that proxies `/api/`, `/proto`, and `/cache/` to the backend and serves the frontend SPA (either with `nginx` or `caddy` as a reverse proxy).

## Secrets

Two secrets are required. Generate them with:

```sh
# JWT signing key (HS256, minimum 32 bytes)
openssl rand -base64 48 > /run/secrets/gradient-jwt

# Database encryption key
openssl rand -base64 48 > /run/secrets/gradient-crypt
```

!!! warning
    Never commit secret files to version control. Use [sops-nix](https://github.com/Mic92/sops-nix) or [agenix](https://github.com/ryantm/agenix) to manage them.

## Server Options

All options live under `services.gradient`.

### General

| Option | Env | Default | Description |
|---|---|---|---|
| `domain` | - | - | Public hostname (required); `GRADIENT_SERVE_URL` is derived from it and `useTls`. |
| `listenAddr` | `GRADIENT_LISTEN_ADDR` | `127.0.0.1` | Bind address. |
| `port` | `GRADIENT_PORT` | `3000` | HTTP port. |
| `baseDir` | `GRADIENT_BASE_DIR` | `/var/lib/gradient` | Data directory. |
| `useTls` | `GRADIENT_USE_TLS` | `true` | Emit `https://` URLs and mark session cookies `Secure`. |
| `useQuic` | `GRADIENT_USE_QUIC` | `false` | Advertise HTTP/3 to clients (`GET /api/v1/config`); the reverse proxy must serve it. |
| `frontend.url` | `GRADIENT_FRONTEND_URL` | `https://<domain>` | Public frontend URL used in CI status links. |
| - | `GRADIENT_STORE_PATH` | - | Nix store path override. |
| - | `GRADIENT_PUBLIC_STATS` | `false` | Expose `GET /api/v1/workers` and worker stats without authentication. |

### `secrets`

| Option | Env | Default | Description |
|---|---|---|---|
| `secrets.jwtFile` | `GRADIENT_SECRETS_JWT_FILE` | - | File with the JWT signing key (required). |
| `secrets.cryptFile` | `GRADIENT_SECRETS_CRYPT_FILE` | - | File with the database encryption key (required). |

### `state`

| Option | Env | Default | Description |
|---|---|---|---|
| `state.validate` | flag `--state-validate` only | `true` | Validate the generated state at build time. |
| `state.delete` | `GRADIENT_STATE_DELETE` | `true` | Remove entities no longer declared in `state`. |
| - | `GRADIENT_STATE_FILE` | - | State file, set by the module. |

See [Declarative State](usage/state.md) for the entities under `state`.

### `registration`, `permissions`, `sentry`, `pullRequests`

| Option | Env | Default | Description |
|---|---|---|---|
| `registration.enable` | `GRADIENT_REGISTRATION_ENABLE` | `true` | Allow self-registration. |
| `permissions.createProject` | `GRADIENT_PERMISSIONS_CREATE_PROJECT` | `everyone` | Who may create projects via the API: `none` (only the declarative state), `superusers` or `everyone`. The frontend hides the button accordingly. |
| `permissions.createCache` | `GRADIENT_PERMISSIONS_CREATE_CACHE` | `everyone` | Same for caches. |
| `sentry.enable` | `GRADIENT_SENTRY_ENABLE` | `false` | Send errors to Sentry. |
| `sentry.dsn` | `GRADIENT_SENTRY_DSN` | `null` | Sentry DSN; `null` reports to the upstream Wavelens instance. |
| `pullRequests.commitName` | `GRADIENT_PULL_REQUESTS_COMMIT_NAME` | `null` | Author name of `open_pr` commits. `null` lets each forge pick attribution: GitHub credits the App bot and signs it verified; Gitea/Forgejo and GitLab use the token owner (needs the `read:user` / `read_user` scope), falling back to `Gradient <gradient@users.noreply.HOST>`. |
| `pullRequests.commitEmail` | `GRADIENT_PULL_REQUESTS_COMMIT_EMAIL` | `null` | Author email of `open_pr` commits. |

### `database`

| Option | Env | Default | Description |
|---|---|---|---|
| `database.url` | `GRADIENT_DATABASE_URL` | `postgresql://gradient@localhost/gradient?host=/run/postgresql` | Connection string. The role must be named explicitly: the local socket authenticates by peer. |
| `database.urlFile` | `GRADIENT_DATABASE_URL_FILE` | from `database.url` | File with the connection string. |
| `database.maxConnections` | `GRADIENT_DATABASE_MAX_CONNECTIONS` | `16` | Scheduler / worker pool size. Total per process is `database.maxConnections + database.web.maxConnections + database.cache.maxConnections`. |
| `database.minConnections` | `GRADIENT_DATABASE_MIN_CONNECTIONS` | `2` | Warm connections in that pool. |
| `database.cache.maxConnections` | `GRADIENT_DATABASE_CACHE_MAX_CONNECTIONS` | `32` | Cache-query pool size, isolated so a prefetch storm cannot starve dispatch. |
| `database.cache.minConnections` | `GRADIENT_DATABASE_CACHE_MIN_CONNECTIONS` | `2` | Warm connections in that pool. |
| `database.web.maxConnections` | `GRADIENT_DATABASE_WEB_MAX_CONNECTIONS` | `8` | HTTP API pool size. |
| `database.web.minConnections` | `GRADIENT_DATABASE_WEB_MIN_CONNECTIONS` | `1` | Warm connections in that pool. |

### `http`

| Option | Env | Default | Description |
|---|---|---|---|
| `http.maxRequestSize` | `GRADIENT_HTTP_MAX_REQUEST_SIZE` | `2097152` (2 MiB) | Max request body for most endpoints. Build-request blobs use a fixed 20 MiB cap. |
| `http.maxSourceUploadSize` | `GRADIENT_HTTP_MAX_SOURCE_UPLOAD_SIZE` | `536870912` (512 MiB) | Max assembled size of a `gradient build` source upload (single-shot, chunked staged total and chunked manifest total). The built-in reverse proxy's `client_max_body_size` follows the largest upload limit. |
| `http.trustedProxies` | `GRADIENT_HTTP_TRUSTED_PROXIES` | loopback | CIDR list of peers allowed to set `X-Forwarded-For`. |
| `http.localIps` | `GRADIENT_HTTP_LOCAL_IPS` | private ranges | CIDR list whose clients receive each cache's `local_priority`. |

### `proto`

| Option | Env | Default | Description |
|---|---|---|---|
| `proto.discoverable` | `GRADIENT_PROTO_DISCOVERABLE` | `true` | Accept incoming `/proto` connections from workers. |
| `proto.federate` | `GRADIENT_PROTO_FEDERATE` | `false` | Accept federated servers on `/proto`; requires `proto.discoverable`. |
| `proto.public` | - | `false` | Expose `/proto` through the reverse proxy. |
| `proto.maxConnections` | `GRADIENT_PROTO_MAX_CONNECTIONS` | `256` | Max simultaneous `/proto` connections; further upgrades get `503` with `Retry-After: 10`. |
| `proto.workerHeartbeatTimeoutSecs` | `GRADIENT_PROTO_WORKER_HEARTBEAT_TIMEOUT_SECS` | `120` | Silence after which a worker is declared dead and its jobs re-queued. Workers heartbeat every 10 s; this is the only detector for a worker lost without a clean TCP close. `0` disables it. |
| `proto.anonymousCache.enable` | `GRADIENT_PROTO_ANONYMOUS_CACHE_ENABLE` | `true` | Allow unauthenticated `GET /cache/{cache}/proto` on public caches. |
| `proto.anonymousCache.maxConnectionsPerIp` | `GRADIENT_PROTO_ANONYMOUS_CACHE_MAX_CONNECTIONS_PER_IP` | `32` | Max anonymous cache connections per client IP. |

### `upload`

| Option | Env | Default | Description |
|---|---|---|---|
| `upload.concurrency` | `GRADIENT_UPLOAD_CONCURRENCY` | `16` | Uploads (NARs and eval-cache blobs) admitted at once; further requests wait, round-robin across workers. |
| `upload.bytesBudget` | `GRADIENT_UPLOAD_BYTES_BUDGET` | `8589934592` (8 GiB) | Sum of admitted upload sizes. An upload that does not fit waits; one larger than the budget runs alone once nothing else is in flight. |
| `upload.leaseIdleSecs` | `GRADIENT_UPLOAD_LEASE_IDLE_SECS` | `300` | Seconds a granted relay upload may go without data before its permit is reclaimed and the worker is told to retry. |

### `nar`

| Option | Env | Default | Description |
|---|---|---|---|
| `nar.maxUploadSize` | `GRADIENT_NAR_MAX_UPLOAD_SIZE` | `536870912` (512 MiB) | Max body of `POST /caches/{cache}/nars`. |
| `nar.smallBytes` | `GRADIENT_NAR_SMALL_BYTES` | `1048576` (1 MiB) | NARs up to this size are relayed through the server and admitted to the hot cache; larger ones use presigned S3 URLs, and NARs over 1 GiB upload as multipart. |
| `nar.hotCacheBytes` | `GRADIENT_NAR_HOT_CACHE_BYTES` | `536870912` (512 MiB) | In-memory NAR cache capacity, ranked by hits per byte. `0` disables it. |
| `nar.verifyDigest` | `GRADIENT_NAR_VERIFY_DIGEST` | `false` | Re-hash NARs committed through presigned S3 uploads. Relayed and REST uploads are always verified. |
| `nar.uploadConcurrency` | `GRADIENT_NAR_UPLOAD_CONCURRENCY` | `4` | Background uploads of relayed NARs from `<baseDir>/nar-staged` to S3. |
| `nar.commitConcurrency` | `GRADIENT_NAR_COMMIT_CONCURRENCY` | `8` | NAR commits (verification, storage placement, cache-index write) running at once. |
| `nar.storageOpenTimeoutSecs` | `GRADIENT_NAR_STORAGE_OPEN_TIMEOUT_SECS` | `60` | Wait for a NAR object stream before answering `NarUnavailable`. |
| `nar.sendChunkTimeoutSecs` | `GRADIENT_NAR_SEND_CHUNK_TIMEOUT_SECS` | `30` | Wait for a `NarPush` chunk to drain before aborting with `NarAbort`. |
| `nar.maxConcurrentServes` | `GRADIENT_NAR_MAX_CONCURRENT_SERVES` | `8` | NAR serving tasks per worker connection. |
| `nar.maxBufferBytes` | `GRADIENT_NAR_MAX_BUFFER_BYTES` | `10737418240` (10 GiB) | Max total of unfinished uploads under `<baseDir>/nar-partial`. |
| `nar.partialTtlSecs` | `GRADIENT_NAR_PARTIAL_TTL_SECS` | `86400` | Age after which an unfinished upload is deleted. `0` disables the cleanup. |

### `cache`

| Option | Env | Default | Description |
|---|---|---|---|
| `cache.upstreamQueryConcurrency` | `GRADIENT_CACHE_UPSTREAM_QUERY_CONCURRENCY` | `32` | Simultaneous narinfo requests to upstream caches server-wide. |
| `cache.maxStorageGb` | `GRADIENT_CACHE_MAX_STORAGE_GB` | `0` | Instance-wide NAR storage cap in GB; when every writable cache of a project has less than 10 MiB left, new evaluations park in `Waiting`. `0` = unlimited; per-cache limits still apply. |
| `cache.signSweepIntervalSecs` | `GRADIENT_CACHE_SIGN_SWEEP_INTERVAL_SECS` | `3600` | Fallback signature backfill interval; uploads are signed immediately. |
| `cache.debugIndexIntervalSecs` | `GRADIENT_CACHE_DEBUG_INDEX_INTERVAL_SECS` | `300` | DWARF build-id index backfill interval; uploads are indexed immediately. |

### `gc`

| Option | Env | Default | Description |
|---|---|---|---|
| `gc.intervalSecs` | `GRADIENT_GC_INTERVAL_SECS` | `3600` | Interval between garbage collection passes. |
| `gc.narTtlHours` | `GRADIENT_GC_NAR_TTL_HOURS` | `336` | Hours a cached path outside the live closure is kept after its last fetch or commit. `gc.narUploadGraceHours` always applies on top. |
| `gc.narUploadGraceHours` | `GRADIENT_GC_NAR_UPLOAD_GRACE_HOURS` | `24` | Grace before an unreferenced NAR object is reclaimed (covers the upload commit window). Also the age past which an unconfirmed `cached_path` with neither staged file nor object is demoted. |
| - | `GRADIENT_GC_ORPHAN_DERIVATION_HOURS` | `24` | Grace before an unreferenced `derivation` row is deleted. |
| `gc.wedgedEvalHours` | `GRADIENT_GC_WEDGED_EVAL_HOURS` | `24` | Hours an evaluation may stay in one phase before it stops blocking evaluation GC. `0` blocks forever. |

### `eval`

| Option | Env | Default | Description |
|---|---|---|---|
| `eval.maxKeep` | `GRADIENT_EVAL_MAX_KEEP` | `30` | Maximum evaluations kept per task. Caps the per-task setting; new tasks start at the lower of `30` and this. `0` disables the cap. |
| `eval.cache.maxTotalBytes` | `GRADIENT_EVAL_CACHE_MAX_TOTAL_BYTES` | `10737418240` (10 GiB) | Total size of shared eval-cache blobs. |
| `eval.cache.maxAgeDays` | `GRADIENT_EVAL_CACHE_MAX_AGE_DAYS` | `30` | Age after which an eval-cache blob is evicted. |
| `eval.cache.sweepIntervalSecs` | `GRADIENT_EVAL_CACHE_SWEEP_INTERVAL_SECS` | `3600` | Interval between eval-cache evictions. |

### `build`

| Option | Env | Default | Description |
|---|---|---|---|
| `build.maxAttempts` | `GRADIENT_BUILD_MAX_ATTEMPTS` | `3` | Attempts before a transient failure becomes `FailedPermanent`. |
| `build.substituteMissEscalationThreshold` | `GRADIENT_BUILD_SUBSTITUTE_MISS_ESCALATION_THRESHOLD` | `2` | Free re-queues of a relay within one evaluation before it is built like any other. A re-queue does not spend an attempt. |
| `build.inputsUnavailableMaxLoops` | `GRADIENT_BUILD_INPUTS_UNAVAILABLE_MAX_LOOPS` | `3` | `InputsUnavailable` self-heal loops per build before it fails fast. |
| `build.retryBackoffSecs` | `GRADIENT_BUILD_RETRY_BACKOFF_SECS` | `30` | Base back-off before retrying a transient failure, doubled per attempt. |
| `build.defaultTimeoutSecs` | `GRADIENT_BUILD_DEFAULT_TIMEOUT_SECS` | `14400` | Timeout for derivations without a `timeout` attribute. `0` disables. |
| `build.defaultMaxSilentSecs` | `GRADIENT_BUILD_DEFAULT_MAX_SILENT_SECS` | `3600` | Silent-output timeout for derivations without `maxSilent`. `0` disables. |

### Build failure states and retries

Builds can fail in three distinct ways:

| Status | Terminal | Meaning |
|---|---|---|
| `FailedPermanent` | Yes | Builder exited non-zero; no retry will be attempted |
| `FailedTransient` | No | Transient error (OOM, disk full, network/substitution failure, builder crash); scheduler will re-queue automatically |
| `FailedTimeout` | Yes | Exceeded `build.defaultTimeoutSecs` or `build.defaultMaxSilentSecs` |

`FailedTransient` is non-terminal: the build is re-queued automatically with an exponential back-off until `build.maxAttempts` is exhausted, at which point the status is promoted to `FailedPermanent`. API entry-point queries treat `FailedTransient` as in-progress; the frontend renders all three variants as "Failed".

Per-derivation `.drv` attributes `timeout`, `maxSilent`, and `preferLocalBuild` override the server defaults when present on a derivation. Note that Nix `meta.*` attributes do **not** propagate to the `.drv`; these must be set as top-level derivation attributes.


### `scheduler`

| Option | Env | Default | Description |
|---|---|---|---|
| `scheduler.scoringPolicy` | `GRADIENT_SCHEDULER_SCORING_POLICY` | `resource-aware` | `simple` weighs path availability, NAR size, dependency count, wait time, builtins and fetch-worker reservation; `resource-aware` adds RAM fit, CPU affinity, `preferLocalBuild` and per-project fair share. Unknown values fall back to `resource-aware`. See [scheduler scoring](development/scheduler-scoring.md). |
| `scheduler.recordCandidates` | `GRADIENT_SCHEDULER_RECORD_CANDIDATES` | `false` | Persist runner-up candidates per dispatch. |
| `scheduler.dispatchRetentionDays` | `GRADIENT_SCHEDULER_DISPATCH_RETENTION_DAYS` | `30` | Retention of `dispatched_job` rows and settled `outbox` rows. `0` keeps them forever. |

### `log`

| Option | Env | Default | Description |
|---|---|---|---|
| `log.level.default` | `GRADIENT_LOG_LEVEL_DEFAULT` | `info` | Level for `gradient_*` targets; dependency noise is pinned to `warn`. `RUST_LOG` overrides everything. |
| `log.level.cache` | `GRADIENT_LOG_LEVEL_CACHE` | `null` | `gradient_cache` override. |
| `log.level.web` | `GRADIENT_LOG_LEVEL_WEB` | `null` | `gradient_web` override. |
| `log.level.proto` | `GRADIENT_LOG_LEVEL_PROTO` | `null` | `gradient_proto` and `gradient_wire` override. |
| `log.level.scheduler` | `GRADIENT_LOG_LEVEL_SCHEDULER` | `null` | `gradient_scheduler` and `gradient_pool` override. |
| `log.chunkBytes` | `GRADIENT_LOG_CHUNK_BYTES` | `262144` (256 KiB) | Target uncompressed size of a stored build-log chunk. |

### `s3`

| Option | Env | Default | Description |
|---|---|---|---|
| `s3.enable` | - | `false` | Store NARs in S3. |
| `s3.bucket` | `GRADIENT_S3_BUCKET` | - | Bucket name; the bucket must not be versioned. |
| `s3.region` | `GRADIENT_S3_REGION` | `us-east-1` | Bucket region. |
| `s3.endpoint` | `GRADIENT_S3_ENDPOINT` | `null` | S3-compatible endpoint (MinIO, R2, ...). |
| `s3.accessKeyId` | `GRADIENT_S3_ACCESS_KEY_ID` | `null` | Access key; `null` uses instance credentials. |
| `s3.secretAccessKeyFile` | `GRADIENT_S3_SECRET_ACCESS_KEY_FILE` | `null` | File with the secret key. |
| `s3.prefix` | `GRADIENT_S3_PREFIX` | `""` | Key prefix inside the bucket. |
| `s3.virtualHostedStyle` | `GRADIENT_S3_VIRTUAL_HOSTED_STYLE` | `false` | Virtual-hosted instead of path-style addressing for a custom endpoint. |
| `s3.readTimeoutSecs` | `GRADIENT_S3_READ_TIMEOUT_SECS` | `60` | Inactivity timeout of an S3 response, reset by every chunk, so large NARs stream as long as they progress. |
| `s3.maxRetries` | `GRADIENT_S3_MAX_RETRIES` | `3` | Retries of a failed request. |
| `s3.retryTimeoutSecs` | `GRADIENT_S3_RETRY_TIMEOUT_SECS` | `250` | Seconds after the first attempt past which no retry starts. Keep it above `(maxRetries + 1) * readTimeoutSecs` and below 5 minutes. |

## Postgres Sizing

Gradient's working set is the build graph, and it is index-bound rather than
table-bound. On the reference deployment a 17 GB database carries 8.5 GB of
indexes, of which `derivation_dependency` alone holds several GB, so a stock 128 MB
`shared_buffers` cannot keep even the hot index set resident and every recursive
graph walk re-reads it from the page cache.

With `postgres.enable = true` the module owns the cluster and sets the sizing
that does not depend on the host, every value a `mkDefault` you can override:

| setting | module default | why |
|---|---|---|
| `random_page_cost` | `1.1` | SSD: a random page costs almost what a sequential one does. At the default of 4 the planner picks bitmap heap scans over the index-only scans the edge tables are built for. |
| `max_connections` | `200` | See below. |
| `max_locks_per_transaction` | `1024` | Graph writes hold one advisory lock per anchor and per dependency they count until they commit. The stock 64 runs out on one large ingest batch ("out of shared memory"); the server logs an error at startup below 256. |

The four settings that scale with the host's RAM have no defensible static
default, so they are options instead. A module that guessed them would size a
2 GB test guest the way it sizes the reference deployment:

| Option | Default | Description |
|--------|---------|-------------|
| `postgres.sharedBuffers` | `"512MB"` | `shared_buffers`: a quarter of the host's RAM, so `"4GB"` on a 16 GB host. The default is a floor, not a target: it is one fixed allocation rather than a per-node one, so it cannot multiply the way the two below can. Raise it on anything larger than 2 GB. |
| `postgres.effectiveCacheSize` | `null` | `effective_cache_size`: three quarters of the host's RAM, so `"12GB"` on a 16 GB host. A planner hint about what the kernel will cache, not an allocation. |
| `postgres.workMem` | `null` | `work_mem`: the floor every ordinary query gets, which the graph walks raise above inside the transaction each one opens. Charged per sort or hash node, so the real ceiling is this times every concurrent query's node count. `"32MB"` suits a host sized for the 80 pooled connections below. |
| `postgres.maintenanceWorkMem` | `null` | `maintenance_work_mem`: index builds and the autovacuum passes over the edge tables. Each of `autovacuum_max_workers` can claim this much at once, so `"1GB"` wants RAM to spare. |

When `database.url` points at a cluster this module does not configure, set the
same seven values there by hand.

`max_connections` has to cover every server process's three pools at once:
`database.maxConnections` plus `database.web.maxConnections` plus
`database.cache.maxConnections` (80 in total by default), with headroom for
`maintenance_work_mem`-sized autovacuum workers and for `psql`. The stock 100 is
enough for one server, not for two.

Two things Gradient handles itself, so they do not belong in the host config. The
edge table (`derivation_dependency`) carries per-table
autovacuum overrides set by migration: all three scale factors go to 0.02, because
these tables are append-heavy and read through index-only scans, and what keeps
those scans index-only is a fresh visibility map rather than a low dead-tuple
count. (`m20260911_000000` sets the same overrides on a third edge table,
`derivation_closure`, which the migration right after it drops with the table.)
And the recursive walks raise `work_mem` to 64 MB with `SET LOCAL` inside their
own transaction, which has to stay above the floor in the table above or it buys
the walk nothing.

## Reverse Proxies

The Gradient server does not come with a built-in http server for the frontend. 
Therefore a reverse proxy / webserver is needed for hosting.
The nixos module provides two preconfigured reverse proxies:
- `nginx`
- `caddy`

### Nginx

| Option | Default | Description |
|--------|---------|-------------|
| `reverseProxy.nginx.enable` | `false` | Whether to enable nginx as the reverse proxy |
| `reverseProxy.nginx.manageTls` | `true` | Let nginx obtain/serve the certificate (`enableACME` + `forceSSL`). Set `false` when an upstream proxy terminates TLS and forwards plain HTTP to nginx; keep `useTls = true` for correct `https://` URLs and secure cookies. No effect when `useTls = false`. |

### Caddy

!!! note
    To match the upstream `services.caddy` configuration you have to manage the ACME host certificate yourself.

| Option | Default | Description |
|--------|---------|-------------|
| `reverseProxy.caddy.enable` | `false` | Whether to enable caddy as the reverse proxy |
| `reverseProxy.caddy.useACMEHost` | `null` | Passed directly to [`services.caddy.virtualHosts.<name>.useACMEHost`](https://search.nixos.org/options?channel=unstable&query=services.caddy.virtualHosts.&show=option:services.caddy.virtualHosts.%3Cname%3E.useACMEHost) |
| `reverseProxy.caddy.extraConfig` | `""` | Caddy config options written to [`services.caddy.virtualHosts.<name>.extraConfig`](https://search.nixos.org/options?channel=unstable&query=services.caddy.virtualHosts.&show=option:services.caddy.virtualHosts.%3Cname%3E.extraConfig) after the reverse proxy setup |

### Custom Reverse Proxy

If you want to use your own reverse proxy you have to setup redirects as follows:
- `https://example.com/api` _(with all subpaths)_ -> `http://${ADDR}:${PORT}/api`
- `https://example.com/proto` -> `http://${ADDR}:${PORT}/proto` _(must support websockets)_
- `https://example.com/cache` _(with all subpaths)_ -> `http://${ADDR}:${PORT}/cache`
All other requests should be handled by a static webserver hosting the files at:
- `${pkgs.gradient-frontend}/share/gradient-frontend`

## Metrics

Set `services.gradient.metrics.tokenFile` to enable `GET /metrics` (Prometheus exposition format). When unset, the endpoint returns 404.

```nix
services.gradient.metrics.tokenFile = "/run/secrets/gradient-metrics";
```

Generate a token with `openssl rand -base64 32`. Configure your Prometheus scraper with `bearer_token_file: /run/secrets/gradient-metrics` (or pass the token directly via `Authorization: Bearer <token>` for ad-hoc curls). The endpoint is rate-limited at 6 req/s with a burst of 5; a 15s scrape interval is comfortable.

The MVP exposes build/evaluation status counts, scheduler queue depth, connected workers, and cache totals. Per-project/cache labels and histograms are tracked as a follow-up.

### Metrics pipeline & retention

The Job Board records build/eval phase timings, dispatch decisions (with scoring breakdown), and worker statistics into dedicated tables. A background task prunes them so they stay bounded:

| Option | Env | Default | Description |
|---|---|---|---|
| `metrics.tokenFile` | `GRADIENT_METRICS_TOKEN_FILE` | `null` | Bearer token file for `GET /metrics`. |
| `metrics.rollupIntervalSecs` | `GRADIENT_METRICS_ROLLUP_INTERVAL_SECS` | `60` | Rollup interval. Each pass also recounts the stored bytes per cache (`cache_usage`) that the dashboard's cache-size tile sums. |
| `metrics.cacheFlushIntervalSecs` | `GRADIENT_METRICS_CACHE_FLUSH_INTERVAL_SECS` | `10` | How often served NAR bytes and counts, accumulated in memory, are added into `cache_metric`. A failed flush costs at most this much telemetry and never a request. |
| `metrics.retention.rawDays` | `GRADIENT_METRICS_RETENTION_RAW_DAYS` | `14` | Retention of raw `phase_event` / `worker_sample` rows (`0` = forever). |
| `metrics.retention.rollupDays` | `GRADIENT_METRICS_RETENTION_ROLLUP_DAYS` | `400` | Retention of minute/hour rollups; day/week are kept (`0` = forever). |
| `metrics.workerSampleIntervalSecs` | `GRADIENT_METRICS_WORKER_SAMPLE_INTERVAL_SECS` | `15` | Worker sampling interval. |
| `metrics.labelTopn` | `GRADIENT_METRICS_LABEL_TOPN` | `20` | Cardinality cap per rollup label dimension. |
| `metrics.instanceIntervalSecs` | `GRADIENT_METRICS_INSTANCE_INTERVAL_SECS` | `30` | InstanceContext window recomputation interval. |
| `metrics.graphConsistencyIntervalSecs` | `GRADIENT_METRICS_GRAPH_CONSISTENCY_INTERVAL_SECS` | `300` | Build-graph consistency sweep interval; violations log as warnings. The sweep is also the only backstop for the moved counters: it recounts `derivation.unwalked_inputs`, `derivation_build.missing_runtime_deps` and `demanded` table-wide, recounts `fetchable` and `unready_deps` over the pending anchors and their direct dependencies, settles the queue against them, names for the live evaluations the pending anchors they reach that no evaluation names any more, and re-heals graph-stuck evaluations. `0` disables all of that. |
| `metrics.otlp.endpoint` | `GRADIENT_METRICS_OTLP_ENDPOINT` | `null` | OTLP collector endpoint (`null` disables). |
| `metrics.otlp.pushIntervalSecs` | `GRADIENT_METRICS_OTLP_PUSH_INTERVAL_SECS` | `30` | OTLP push interval. |

See also `scheduler.recordCandidates` and `scheduler.dispatchRetentionDays`.

## OIDC

```nix
services.gradient.oidc = {
  enable           = true;
  required         = false;   # set true to disable username/password login and require OIDC for all users
  clientId         = "gradient";
  clientSecretFile = "/run/secrets/gradient-oidc-secret";
  discoveryUrl     = "https://auth.example.com";
  scopes           = [ "openid" "email" "profile" ];
  iconUrl          = null;    # optional provider logo URL
};
```

Gradient uses PKCE (S256) and discovers all provider endpoints from `discoveryUrl/.well-known/openid-configuration` and callback url is at `https://$domain/api/v1/auth/oidc/callback`. Set `required` to `true` to disable basic auth and require OIDC for all users. Because PKCE is sent on every request, providers that gate it (e.g. kanidm) do not need `allowInsecureClientDisablePkce`.

To map OIDC groups to project roles, request the `groups` scope (add `"groups"` to `scopes`) so the ID token carries the user's group claims, then attach `oidc_group` lists to state-managed roles (see [Declarative State](usage/state.md)).

## SCIM

```nix
services.gradient.scim = {
  enable     = true;
  tokenFile  = "/run/secrets/gradient-scim-token";
  hardDelete = false;   # default: DELETE soft-disables (active=false)
};
```

Enabling SCIM mounts an instance-level SCIM 2.0 provisioning surface at `https://$domain/scim/v2`, authenticated by the bearer token in `tokenFile` (not user credentials). SCIM provisions passwordless `managed` users that later authenticate via OIDC; SCIM groups map to roles through `scim_group` (see [SCIM](usage/scim.md) and [Declarative State](usage/state.md)).

| Option | Env | Default | Description |
|---|---|---|---|
| `scim.enable` | `GRADIENT_SCIM_ENABLE` | `false` | Mount the `/scim/v2` provisioning endpoints |
| `scim.tokenFile` | `GRADIENT_SCIM_TOKEN_FILE` | - | Path to the file holding the SCIM bearer token (required when enabled) |
| `scim.hardDelete` | `GRADIENT_SCIM_HARD_DELETE` | `false` | Hard-delete (cascade) on `DELETE /Users/{id}`; default soft-disables (`active=false`) |

## Email

```nix
services.gradient.email = {
  enable              = true;
  requireVerification = true;
  smtp = {
    host         = "smtp.example.com";
    port         = 587;
    username     = "gradient@example.com";
    passwordFile = "/run/secrets/gradient-smtp";
  };
  from = {
    address = "gradient@example.com";
    name    = "Gradient";
  };
};
```

## GitHub App

A GitHub App provides automatic webhook delivery and CI status reporting without per-task tokens. One App covers all projects on the instance.

### Setup

1. Create a GitHub App at `github.com -> Settings -> Developer settings -> GitHub Apps -> New GitHub App`.
   - **Webhook URL**: `https://gradient.example.com/api/v1/hooks/github`
   - **Webhook secret**: generate a random value and note it
   - **Permissions**: Repository -> Commit statuses (Read & Write), Repository -> Contents (Read)
   - **Subscribe to events**: Push, Installation

2. After creation note the **App ID** and download the **private key** PEM.

3. Configure Gradient:

```nix
services.gradient.githubApp = {
  enable             = true;
  id                 = 123456;
  privateKeyFile     = "/run/secrets/gradient-github-app-key";
  webhookSecretFile  = "/run/secrets/gradient-github-app-webhook-secret";
};
```

4. Install the App on each GitHub organization. Gradient auto-creates the `github-<account>` integration pair from the install webhook. Alternatively, project admins can create integrations manually via the UI by entering the installation id (one per GitHub account; multiple per project are supported).

5. Once installed, push events automatically trigger evaluations (no polling) and CI statuses are reported using the installation token instead of a per-task PAT.

## Forge Webhooks (Gitea / Forgejo / GitLab / GitHub without App)

For non-GitHub forges or GitHub without the App, configure a per-project webhook secret via the UI:

1. Open **Project -> Settings -> Forge Webhooks** and click **Generate Webhook Secret**.
2. Copy the displayed **Webhook URL** and **Secret**.
3. In your forge, create a push webhook pointing to the URL, using the secret for HMAC-SHA256 signing.

Forge path by type:

| Forge | URL path segment | Signature header |
|---|---|---|
| Gitea / Forgejo | `/hooks/gitea/{project}` or `/hooks/forgejo/{project}` | `X-Gitea-Signature` |
| GitLab | `/hooks/gitlab/{project}` | `X-Gitlab-Token` |
| GitHub (no App) | `/hooks/github/{project}` | `X-Hub-Signature-256` |

Gradient matches the incoming push payload's clone URL against active tasks and queues an evaluation immediately.

## Workers

Build capacity is provided by **`gradient-worker`** instances that connect to the server over a WebSocket at `/proto`. Workers are separate processes and can run on the same host or on dedicated build machines.

The server does **not** start a worker automatically. Configure one explicitly using the `gradient-worker` NixOS module.

### Co-located Worker

A worker on the server's own machine needs no credentials of its own:

```nix
services.gradient.worker = {
  enable = true;
  build.metrics = true; # opt in to per-build resource metrics for smarter scheduling (enables Nix's cgroups experimental feature)
};
```

That is the whole configuration. `services.gradient.localWorker` defaults to
`services.gradient.worker.enable`, and in that mode the server module does the
registration work itself:

- derives a stable worker UUID from the hostname, so the server can
  pre-register it before either service has ever run
- generates a 48-byte token on first start into `/var/lib/gradient-worker/local-token`
  (mode 0400, owned by `gradient-worker`) and writes the matching peers file
- registers the worker in `state.workers` as an `auto_enable` base worker, so
  every project - including ones created later - picks it up without a
  registration step in the web UI

Both services read the token through systemd's `LoadCredential`, which resolves
it as root, so no shared group is needed. The token survives reboots and is
never regenerated; delete the file and restart to rotate it.

!!! note
    A worker is only authorized for projects that have a cache subscribed, and a
    base worker no project has enabled yet is refused outright. On a brand-new
    instance the worker therefore sits in its reconnect backoff (60s ceiling)
    until a project exists *and* has a cache; it joins on the next attempt.

Set `services.gradient.localWorker = false` to opt out and configure
`worker.id` / `worker.peersFile` by hand, exactly like a remote worker.

### Remote Workers

Deploy `gradient-worker` on dedicated build machines. First register the worker under a project - either declaratively via `state.workers` (see below) or via the API. The `worker_id` must be a **UUID v4**. The worker auto-generates one on first start and persists it to `/var/lib/gradient-worker/worker-id`:

```sh
cat /var/lib/gradient-worker/worker-id
```

```sh
curl -X POST https://gradient.example.com/api/v1/projects/myproject/workers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"worker_id": "550e8400-e29b-41d4-a716-446655440001"}'
# -> {"error":false,"message":{"peer_id":"<uuid>","token":"<token>"}}
```

You can optionally pre-generate the token and pass it in the request (`openssl rand -base64 48`); the response will then omit the token field.

Then on the build machine:

```nix
imports = [ inputs.gradient.nixosModules.gradient-worker ];

services.gradient.worker = {
  enable    = true;
  serverUrl = "wss://gradient.example.com/proto";
  peersFile = "/run/secrets/gradient-worker-peers";

  capabilities = {
    fetch = true;
    eval  = true;
    build = true;
  };

  build = {
    maxConcurrent = 8;
    metrics       = true; # opt in to per-build resource metrics for smarter scheduling (enables Nix's cgroups experimental feature)
  };
  eval = {
    workers       = 2;
    maxConcurrent = 2;
  };
};
```

Write the registration result to the peers file (one `peer_id:token` pair per line):

```sh
echo "<peer_id>:<token>" > /run/secrets/gradient-worker-peers
```

The special peer ID `*` can be used instead of a specific UUID to respond with that token for any peer the server challenges:

```text
# /run/secrets/gradient-worker-peers
*:<token>
```

The token must be the 48-byte random secret returned by the registration API (generated via `openssl rand -base64 48` server-side).


### Worker Options

All options live under `services.gradient.worker`; envs carry the `GRADIENT_WORKER_` prefix.

| Option | Env | Default | Description |
|---|---|---|---|
| `serverUrl` | `GRADIENT_WORKER_SERVER_URL` | `null` | WebSocket URL of the server's `/proto` endpoint. |
| `id` | `GRADIENT_WORKER_ID` | `null` | Worker UUID; `null` reads or generates `<baseDir>/worker-id`. |
| `peersFile` | `GRADIENT_WORKER_PEERS_FILE` | `null` | Peers file (`peer_id:token` per line, `*` = any peer). |
| `baseDir` | `GRADIENT_WORKER_BASE_DIR` | `/var/lib/gradient-worker` | State directory. |
| `discoverable` | `GRADIENT_WORKER_DISCOVERABLE` | `false` | Accept incoming connections from the server. |
| `listenAddr` | `GRADIENT_WORKER_LISTEN_ADDR` | `127.0.0.1` | Listener bind address. |
| `port` | `GRADIENT_WORKER_PORT` | `3100` | Listener port. |
| `drainTimeoutSecs` | `GRADIENT_WORKER_DRAIN_TIMEOUT_SECS` | `60` | How long a stop waits for running jobs; the worker stops accepting work, sends `Draining`, finishes and reports what runs, then exits. `TimeoutStopSec` is `drainTimeoutSecs + 30`; `0` waits without limit. A second signal skips the wait. |
| `gcrootsDir` | `GRADIENT_WORKER_GCROOTS_DIR` | `/nix/var/nix/gcroots/gradient` | Indirect GC roots pinning each running build. Empty disables. |
| `packages.nix` / `packages.ssh` | `GRADIENT_WORKER_NIX_BIN` / `GRADIENT_WORKER_SSH_BIN` | fork / OpenSSH | Binaries used for evaluation, fetching and private inputs. |
| `capabilities.fetch` | `GRADIENT_WORKER_CAPABILITIES_FETCH` | `true` | Prefetch flake inputs. |
| `capabilities.eval` | `GRADIENT_WORKER_CAPABILITIES_EVAL` | `true` | Run evaluations. |
| `capabilities.build` | `GRADIENT_WORKER_CAPABILITIES_BUILD` | `true` | Run builds. |
| `capabilities.federate` | `GRADIENT_WORKER_CAPABILITIES_FEDERATE` | `false` | Relay work and NARs; requires `discoverable`. |
| `system.architectures` | `GRADIENT_WORKER_SYSTEM_ARCHITECTURES` | host system | Systems this worker builds for. |
| `system.features` | `GRADIENT_WORKER_SYSTEM_FEATURES` | detected | System features; empty detects them from `nix config show system-features`. |
| `system.cpuCoreScore` | `GRADIENT_WORKER_SYSTEM_CPU_CORE_SCORE` | `null` | Single-core speed score; `null` benchmarks at startup. |
| `system.minFreeRamMb` | `GRADIENT_WORKER_SYSTEM_MIN_FREE_RAM_MB` | `0` (adaptive) | Free-RAM margin of the eval reaper: below it, the one eval subprocess large enough to restore the margin is killed and its eval reported failed. `0` = 10% of RAM clamped to 128 MiB to 1 GiB. |
| `nixDaemon.maxConnections` | `GRADIENT_WORKER_NIX_DAEMON_MAX_CONNECTIONS` | `build.maxConcurrent * 9 + 16` | Local nix-daemon pool size. |
| `eval.maxConcurrent` | `GRADIENT_WORKER_EVAL_MAX_CONCURRENT` | `1` | Parallel evaluations. |
| `eval.workers` | `GRADIENT_WORKER_EVAL_WORKERS` | `8` | Evaluator subprocesses. |
| `eval.forkWorkers` | `GRADIENT_WORKER_EVAL_FORK_WORKERS` | `null` | Eval pool size; `null` sizes to the core count capped at 16, bounded so `forkWorkers * maxRss` fits RAM. |
| `eval.maxRss` | `GRADIENT_WORKER_EVAL_MAX_RSS` | `8589934592` (8 GiB) | RSS above which an eval subprocess is recycled between calls. |
| `eval.metrics` | `GRADIENT_WORKER_EVAL_METRICS` | `true` | Collect per-evaluation Nix statistics. |
| `eval.cache.dir` | `GRADIENT_WORKER_EVAL_CACHE_DIR` | `null` | Eval cache directory (`NIX_CACHE_HOME`); `null` = `<baseDir>/eval-cache`. |
| `eval.cache.share` | `GRADIENT_WORKER_EVAL_CACHE_SHARE` | `true` | Share eval-cache blobs across workers. |
| `build.maxConcurrent` | `GRADIENT_WORKER_BUILD_MAX_CONCURRENT` | `32` | Parallel build slots. |
| `build.maxCores` | `GRADIENT_WORKER_BUILD_MAX_CORES` | `null` | Cores per build (`--cores`); `null` = all. |
| `build.metrics` | `GRADIENT_WORKER_BUILD_METRICS` | `false` | Record per-build peak RAM, CPU time and disk I/O for the resource-aware scheduler. Enables Nix's experimental `cgroups` feature and `use-cgroups`, delegates cgroup controllers to `nix-daemon.service`, and defaults `nix.package` to `packages.nix`. |
| `build.cgroupRoot` | `GRADIENT_WORKER_BUILD_CGROUP_ROOT` | `/sys/fs/cgroup/system.slice/nix-daemon.service` | Daemon cgroup containing the per-build cgroups. |
| `nar.maxConcurrentUploads` | `GRADIENT_WORKER_NAR_MAX_CONCURRENT_UPLOADS` | `8` | Object-storage PUTs in flight; throttled or failed PUTs retry up to 6 times with jittered backoff. |
| `nar.partialTtlSecs` | `GRADIENT_WORKER_NAR_PARTIAL_TTL_SECS` | `86400` | Age after which an unfinished download under `<baseDir>/nar-partial` is deleted. `0` disables. |
| `log.level.default` | `GRADIENT_WORKER_LOG_LEVEL_DEFAULT` | `info` | Worker log level. |
| `log.level.eval` / `.build` / `.proto` | `GRADIENT_WORKER_LOG_LEVEL_EVAL` / `_BUILD` / `_PROTO` | `null` | Per-component overrides. |
| `log.burstBytesPerMin` | `GRADIENT_WORKER_LOG_BURST_BYTES_PER_MIN` | `8388608` (8 MiB) | Build-log bytes forwarded per build per minute; past it a truncation marker is appended and forwarding stops. |
| `log.sustainedBytesPerHour` | `GRADIENT_WORKER_LOG_SUSTAINED_BYTES_PER_HOUR` | `67108864` (64 MiB) | Build-log bytes forwarded per build per hour. |
| `log.fetchFromStore` | `GRADIENT_WORKER_LOG_FETCH_FROM_STORE` | `true` | Forward the stored log of an already-built derivation. |

### Hashing

Gradient hashes NARs and compressed cache files with **SHA-256** by default. No client-side experimental feature is required to substitute from a Gradient cache.

BLAKE3-prefixed (`blake3:{nix32}`) hashes are still accepted on the read path so narinfo rows uploaded while the BLAKE3 default was active (issue #132) keep resolving, and so upstream caches that advertise either algorithm interoperate cleanly.

## Declarative State

Users, projects, tasks, integrations, caches, API keys, custom roles, and workers can be declared in `services.gradient.state` and reconciled on every startup. See [Declarative State](usage/state.md) for the full options reference.

### API keys

State-managed API keys are declared under `state.api_keys.<name>`:

- `key_file` (required, path): file containing the lowercase 64-char SHA-256
  hex digest of the token (without the `GRAD` prefix).
- `owned_by` (required, string): username that owns the key.
- `permissions` (required, list of strings): permission identifiers the key
  grants. See `gradient_db::permissions::Permission` (or
  `GET /user/keys/permissions`) for the full list.
- `project` (optional, string): project name to pin the key to.
  Omit for an unscoped key.

### Roles

State-managed custom roles are declared under `state.roles.<name>`:

- `name` (defaults to attrset key): role name. Must be unique within the
  project and must not collide with built-in role names
  (`Admin`, `Write`, `View`).
- `project` (required, string): the project this role belongs to.
- `permissions` (required, list of strings): the capabilities the role grants.

Managed roles cannot be modified or deleted via the API.

### Flake input overrides

Each task may declare per-input flake overrides applied during evaluation fetch. Each entry must set exactly one of:

- `url` - a flake-ref string to replace the input's URL.
- `keep_url = true` - force an update of the input keeping the URL declared in the task's `flake.nix`.

Empty `flake_input_overrides = {}` (the default) means no overrides - `flake.lock` is used as-is. Setting the attrset to `{}` from a non-empty state removes all override rows for that task.

This is a persistent, task-wide mechanism. For a one-off override on a single build request, use `gradient build`'s [`--override-input`](usage/cli.md#build-requests) instead.

```nix
services.gradient.state.tasks.my-task = {
  # ...
  flake_input_overrides = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.keep_url = true;
  };
};
```
