# Configuration

Gradient is configured exclusively through its **NixOS module**. There is no configuration file or command-line flags - all options are set in your NixOS configuration under `services.gradient`.

## Minimal Setup

```nix
services.gradient = {
  enable            = true;
  frontend.enable   = true;
  configurePostgres = true;
  reverseProxy.nginx.enable = true;
  domain            = "gradient.example.com";
  jwtSecretFile     = "/run/secrets/gradient-jwt";
  cryptSecretFile   = "/run/secrets/gradient-crypt";
};
```

`configurePostgres` creates a local PostgreSQL database and user. `reverseProxy` adds a virtual host that proxies `/api/`, `/proto`, and `/cache/` to the backend and serves the frontend SPA (either with `nginx` or `caddy` as a reverse proxy)

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

## All Options

| Option | Default | Description |
|---|---|---|
| `domain` | - | Public hostname (required) |
| `baseDir` | `/var/lib/gradient` | Data directory |
| `listenAddr` | `127.0.0.1` | Bind address |
| `port` | `3000` | HTTP port |
| `jwtSecretFile` | - | Path to JWT secret file (required) |
| `cryptSecretFile` | - | Path to encryption secret file (required) |
| `metricsTokenFile` | `null` | When set, enables `GET /metrics` (Prometheus exposition). The file's contents must be presented as `Authorization: Bearer <token>` by scrapers. When `null`, the endpoint returns 404. |
| `databaseUrlFile` | auto | Override the PostgreSQL connection string file |
| `databaseMaxConnections` | `32` | Max connections in the scheduler / worker / cache pool (`GRADIENT_DATABASE_MAX_CONNECTIONS`). Total per process is `databaseMaxConnections + databaseWebMaxConnections`; raise only if Postgres `max_connections` has headroom. |
| `databaseMinConnections` | `2` | Min connections kept warm in the scheduler / worker / cache pool (`GRADIENT_DATABASE_MIN_CONNECTIONS`). |
| `databaseWebMaxConnections` | `16` | Max connections in the axum HTTP pool (`GRADIENT_DATABASE_WEB_MAX_CONNECTIONS`). |
| `databaseWebMinConnections` | `1` | Min connections kept warm in the axum HTTP pool (`GRADIENT_DATABASE_WEB_MIN_CONNECTIONS`). |
| `reportErrors` | `false` | Send errors to Sentry |
| `settings.sentryDsn` | `null` | Override the Sentry DSN used when `reportErrors` is true. `null` ships reports to the upstream Wavelens instance at `reports.wavelens.io`; set your own DSN to keep reports in-house. (`GRADIENT_SENTRY_DSN`) |
| `discoverable` | `true` | Accept incoming `/proto` WebSocket connections from workers |
| `settings.maxProtoConnections` | `256` | Max simultaneous worker WebSocket connections; further upgrades return `503 Service Unavailable` with `Retry-After: 10` until a slot frees |
| `settings.keepEvaluations` | `30` | Global maximum of evaluations kept per project (caps the per-project setting) |
| `settings.logChunkBytes` | `262144` (256 KiB) | Target uncompressed size for each zstd build-log chunk written on finalize. Chunks split on line boundaries, so an over-long line may exceed this. (`GRADIENT_LOG_CHUNK_BYTES`) |
| `settings.maxStorageGb` | `0` | Instance-wide cap on total cached NAR storage, in GB. When all writable caches for an org have less than 10 MiB headroom, new evaluations park in `Waiting`. `0` = unlimited; per-cache `max_storage_gb` limits still apply. (`GRADIENT_MAX_STORAGE_GB`) |
| `settings.evalCacheMaxTotalBytes` | `10737418240` (10 GiB) | Total byte cap for fleet-shared eval-cache blobs. The eviction sweep drops oldest-`updated_at` rows until the surviving total is at or under this. (`GRADIENT_EVAL_CACHE_MAX_TOTAL_BYTES`) |
| `settings.evalCacheMaxAgeDays` | `30` | Max age in days for an eval-cache blob; older blobs are evicted by the sweep regardless of the size cap. (`GRADIENT_EVAL_CACHE_MAX_AGE_DAYS`) |
| `settings.evalCacheSweepIntervalSecs` | `3600` | Interval in seconds between eval-cache eviction sweeps. (`GRADIENT_EVAL_CACHE_SWEEP_INTERVAL_SECS`) |
| `settings.maxRequestSize` | `2097152` (2 MiB) | Max HTTP request body in bytes for most endpoints (caps webhook/JSON payloads to prevent OOM). The build-request blob endpoint uses a fixed 20 MiB cap. |
| `settings.maxNarUploadSize` | `536870912` (512 MiB) | Max body in bytes for `POST /caches/{cache}/nars`; overrides the general `maxRequestSize` cap for NAR uploads. (`GRADIENT_MAX_NAR_UPLOAD_SIZE`) |
| `settings.logLevel.default` | `info` | Log level: `trace` `debug` `info` `warn` `error` |
| `settings.logLevel.cache` | null | Cache log level override (null inherits default) |
| `settings.logLevel.web` | null | Web log level override (null inherits default) |
| `settings.logLevel.proto` | null | Proto log level override (null inherits default) |
| `settings.enableRegistration` | `true` | Allow new user self-registration |
| `settings.deleteState` | `true` | Remove entities no longer in `state` (see below) |
| `settings.cacheTtlHours` | `336` | TTL in hours for cached NARs not fetched recently (0 = disabled) |
| `settings.narStorageOpenTimeoutSecs` | `60` | Max seconds the server will wait to open a NAR object stream (e.g. an S3 GET) before emitting `NarUnavailable`. Caps how long a stalled storage backend can block a `NarRequest`. |
| `settings.narSendChunkTimeoutSecs` | `30` | Max seconds a single outbound `NarPush` chunk may sit in the per-connection writer queue waiting for the WebSocket sink to drain before the transfer is aborted with `NarAbort`. |
| `settings.maxConcurrentNarServes` | `8` | Max NAR-serving tasks running concurrently per worker connection. Bounds memory and storage-backend fan-out when a worker requests many paths in a single batch. |
| `settings.maxNarBufferBytes` | `10737418240` (10 GiB) | Max total bytes the server may hold across open `*.partial` NAR upload files (un-finalised `NarPush` streams staged under `<baseDir>/nar-partial`). Prevents a rogue worker from filling the disk by opening many uploads without finalising them (issue #109). |
| `settings.narPartialTtlSecs` | `86400` (24 h) | TTL in seconds for partially-received NAR uploads (`*.partial`) staged under `<baseDir>/nar-partial`. A periodic sweep deletes partials older than this so an abandoned resumable transfer can't pin disk forever (issue #225). `0` disables the sweep. (`GRADIENT_NAR_PARTIAL_TTL_SECS`) |
| `settings.allowAnonymousCache` | `true` | Allow unauthenticated clients to use `GET /cache/{cache}/proto` for public caches. When `false`, anonymous handshakes are rejected with 403. Private caches always require an API key regardless. (`GRADIENT_PROTO_ALLOW_ANONYMOUS_CACHE`) |
| `settings.anonMaxConnectionsPerIp` | `32` | Maximum simultaneous anonymous `/cache/proto` connections per client IP. (`GRADIENT_PROTO_ANON_MAX_CONNECTIONS_PER_IP`) |
| `settings.anonRatePerSecond` | `20` | Sustained request rate (per second) allowed for an anonymous proto session. (`GRADIENT_PROTO_ANON_RATE_PER_SECOND`) |
| `settings.anonRateBurst` | `200` | Burst capacity for the anonymous proto session token bucket. (`GRADIENT_PROTO_ANON_RATE_BURST`) |
| `settings.trustedProxies` | `127.0.0.1/32,::1/128` | Comma-separated CIDR allowlist of peers permitted to set `X-Forwarded-For` (`GRADIENT_TRUSTED_PROXIES`). |
| `settings.localIps` | `10.0.0.0/8` | Comma-separated CIDR allowlist whose resolved client IPs receive each cache's `local_priority` value (`GRADIENT_LOCAL_IPS`). |
| `settings.buildMaxAttempts` | `3` | Maximum number of build attempts before a transient failure is promoted to `FailedPermanent`. (`GRADIENT_BUILD_MAX_ATTEMPTS`) |
| `settings.substituteMissEscalationThreshold` | `2` | Substitute attempts before a substitutable build escalates to a real arch-bound build. (`GRADIENT_SUBSTITUTE_MISS_ESCALATION_THRESHOLD`) |
| `settings.buildRetryBackoffSecs` | `30` | Base back-off in seconds before retrying a transient build failure; doubled after each prior attempt (exponential). (`GRADIENT_BUILD_RETRY_BACKOFF_SECS`) |
| `settings.buildDefaultTimeoutSecs` | `14400` | Default wall-clock timeout (seconds) for builds whose `.drv` does not set a `timeout` attribute. `0` disables. (`GRADIENT_BUILD_DEFAULT_TIMEOUT_SECS`) |
| `settings.buildDefaultMaxSilentSecs` | `3600` | Default silent-output timeout (seconds) for builds whose `.drv` does not set a `maxSilent` attribute. `0` disables. (`GRADIENT_BUILD_DEFAULT_MAX_SILENT_SECS`) |
| `settings.schedulerScoringPolicy` | `resource-aware` | Scheduler scoring policy ranking queued jobs against a requesting worker (`GRADIENT_SCHEDULER_SCORING_POLICY`). Values: `simple`, `resource-aware`. `simple` is the basic rule set, weighing path availability, NAR size, dependency count, wait-time anti-starvation, builtin de-prioritization and fetch-worker reservation. `resource-aware` adds RAM/OOM-fit, CPU affinity, preferLocalBuild affinity and per-org fair-share on top, and is the default. Unknown values fall back to `resource-aware`. See [scheduler scoring](development/scheduler-scoring.md). |

### Build failure states and retries

Builds can fail in three distinct ways:

| Status | Terminal | Meaning |
|---|---|---|
| `FailedPermanent` | Yes | Builder exited non-zero; no retry will be attempted |
| `FailedTransient` | No | Transient error (OOM, disk full, network/substitution failure, builder crash); scheduler will re-queue automatically |
| `FailedTimeout` | Yes | Exceeded `buildDefaultTimeoutSecs` or `buildDefaultMaxSilentSecs` |

`FailedTransient` is non-terminal: the build is re-queued automatically with an exponential back-off until `buildMaxAttempts` is exhausted, at which point the status is promoted to `FailedPermanent`. API entry-point queries treat `FailedTransient` as in-progress; the frontend renders all three variants as "Failed".

Per-derivation `.drv` attributes `timeout`, `maxSilent`, and `preferLocalBuild` override the server defaults when present on a derivation. Note that Nix `meta.*` attributes do **not** propagate to the `.drv`; these must be set as top-level derivation attributes.

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

### Caddy

!!! note
    To match the upstream `services.caddy` configuration you have to manage the ACME host certificate yourself.

| Option | Default | Description |
|--------|---------|-------------|
| `reverseProxy.caddy.enable` | `false` | Whether to enable caddy as the reverse proxy |
| `reverseProxy.caddy.useACMEHost` | `null` | Passed directly to [`services.caddy.virtualHosts.<name>.useACMEHost`](https://search.nixos.org/options?channel=unstable&query=services.caddy.virtualHosts.&show=option:services.caddy.virtualHosts.%3Cname%3E.useACMEHost) |
| `reversePorxy.caddy.extraConfig` | `""` | Caddy config options written to [`services.caddy.virtualHosts.<name>.extraConfig`](https://search.nixos.org/options?channel=unstable&query=services.caddy.virtualHosts.&show=option:services.caddy.virtualHosts.%3Cname%3E.extraConfig) after the reverse proxy setup |

### Custom Reverse Proxy

If you want to use your own reverse proxy you have to setup redirects as follows:
- `https://example.com/api` _(with all subpaths)_ -> `http://${ADDR}:${PORT}/api`
- `https://example.com/proto` -> `http://${ADDR}:${PORT}/proto` _(must support websockets)_
- `https://example.com/cache` _(with all subpaths)_ -> `http://${ADDR}:${PORT}/cache`
All other requests should be handled by a static webserver hosting the files at:
- `${pkgs.gradient-frontend}/share/gradient-frontend`

## Metrics

Set `services.gradient.metricsTokenFile` to a file path to enable `GET /metrics` (Prometheus exposition format). When unset, the endpoint returns 404.

```nix
services.gradient.metricsTokenFile = "/run/secrets/gradient-metrics";
```

Generate a token with `openssl rand -base64 32`. Configure your Prometheus scraper with `bearer_token_file: /run/secrets/gradient-metrics` (or pass the token directly via `Authorization: Bearer <token>` for ad-hoc curls). The endpoint is rate-limited at 6 req/s with a burst of 5; a 15s scrape interval is comfortable.

The MVP exposes build/evaluation status counts, scheduler queue depth, connected workers, and cache totals. Per-org/cache labels and histograms are tracked as a follow-up.

### Metrics pipeline & retention

The Job Board records build/eval phase timings, dispatch decisions (with scoring breakdown), and worker statistics into dedicated tables. A background task prunes them so they stay bounded; all settings live under `services.gradient.settings`:

| Option / env var | Default | Purpose |
| --- | --- | --- |
| `metricsRollupIntervalSecs` / `GRADIENT_METRICS_ROLLUP_INTERVAL` | 60 | Rollup-aggregator pass interval. |
| `metricsRetentionRawDays` / `GRADIENT_METRICS_RETENTION_RAW_DAYS` | 14 | Retention for raw `phase_event` / `worker_sample` rows (0 = forever). |
| `metricsRetentionRollupDays` / `GRADIENT_METRICS_RETENTION_ROLLUP_DAYS` | 400 | Retention for minute/hour rollups; day/week kept (0 = forever). |
| `dispatchRetentionDays` / `GRADIENT_DISPATCH_RETENTION_DAYS` | 30 | Retention for `dispatched_job` forensic rows (0 = forever). |
| `workerSampleIntervalSecs` / `GRADIENT_WORKER_SAMPLE_INTERVAL` | 15 | Worker live-metric sampling interval. |
| `metricsLabelTopn` / `GRADIENT_METRICS_LABEL_TOPN` | 20 | Per-dimension cardinality cap for rollup labels. |
| `otlpEndpoint` / `GRADIENT_OTLP_ENDPOINT` | null | OTLP collector endpoint for metric push (null disables). |
| `otlpPushIntervalSecs` / `GRADIENT_OTLP_PUSH_INTERVAL` | 30 | OTLP push interval. |
| `dispatchRecordCandidates` / `GRADIENT_DISPATCH_RECORD_CANDIDATES` | false | Persist runner-up scoring candidates per dispatch. |
| `instanceMetricsIntervalSecs` / `GRADIENT_INSTANCE_METRICS_INTERVAL` | 30 | InstanceContext window recomputation interval. |

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

To map OIDC groups to organization roles, request the `groups` scope (add `"groups"` to `scopes`) so the ID token carries the user's group claims, then attach `oidc_group` lists to state-managed roles (see [Declarative State](usage/state.md)).

## Email

```nix
services.gradient.email = {
  enable              = true;
  requireVerification = true;
  smtpHost            = "smtp.example.com";
  smtpPort            = 587;
  smtpUsername        = "gradient@example.com";
  smtpPasswordFile    = "/run/secrets/gradient-smtp";
  fromAddress         = "gradient@example.com";
  fromName            = "Gradient";
};
```

## GitHub App

A GitHub App provides automatic webhook delivery and CI status reporting without per-project tokens. One App covers all organizations on the instance.

### Setup

1. Create a GitHub App at `github.com → Settings → Developer settings → GitHub Apps → New GitHub App`.
   - **Webhook URL**: `https://gradient.example.com/api/v1/hooks/github`
   - **Webhook secret**: generate a random value and note it
   - **Permissions**: Repository → Commit statuses (Read & Write), Repository → Contents (Read)
   - **Subscribe to events**: Push, Installation

2. After creation note the **App ID** and download the **private key** PEM.

3. Configure Gradient:

```nix
services.gradient.githubApp = {
  enable             = true;
  appId              = 123456;
  privateKeyFile     = "/run/secrets/gradient-github-app-key";
  webhookSecretFile  = "/run/secrets/gradient-github-app-webhook-secret";
};
```

4. Install the App on each GitHub organization. Gradient auto-stores the `installation_id` from the webhook.

5. Once installed, push events automatically trigger evaluations (no polling) and CI statuses are reported using the installation token instead of a per-project PAT.

## Forge Webhooks (Gitea / Forgejo / GitLab / GitHub without App)

For non-GitHub forges or GitHub without the App, configure a per-organization webhook secret via the UI:

1. Open **Organization → Settings → Forge Webhooks** and click **Generate Webhook Secret**.
2. Copy the displayed **Webhook URL** and **Secret**.
3. In your forge, create a push webhook pointing to the URL, using the secret for HMAC-SHA256 signing.

Forge path by type:

| Forge | URL path segment | Signature header |
|---|---|---|
| Gitea / Forgejo | `/hooks/gitea/{org}` or `/hooks/forgejo/{org}` | `X-Gitea-Signature` |
| GitLab | `/hooks/gitlab/{org}` | `X-Gitlab-Token` |
| GitHub (no App) | `/hooks/github/{org}` | `X-Hub-Signature-256` |

Gradient matches the incoming push payload's clone URL against active projects and queues an evaluation immediately.

## Workers

Build capacity is provided by **`gradient-worker`** instances that connect to the server over a WebSocket at `/proto`. Workers are separate processes and can run on the same host or on dedicated build machines.

The server does **not** start a worker automatically. Configure one explicitly using the `gradient-worker` NixOS module.

### Co-located Worker

To run a worker on the same machine as the server, import the worker module and configure `services.gradient.worker`:

```nix
imports = [ inputs.gradient.nixosModules.gradient-worker ];

services.gradient.worker = {
  enable    = true;
  serverUrl = "ws://127.0.0.1:3000/proto";
  capabilities = {
    fetch = true;
    eval  = true;
    build = true;
    sign  = true;
  };
  settings.buildMetrics = true; # opt in to per-build resource metrics for smarter scheduling (enables Nix's cgroups experimental feature)
};
```

### Remote Workers

Deploy `gradient-worker` on dedicated build machines. First register the worker under an organization - either declaratively via `state.workers` (see below) or via the API. The `worker_id` must be a **UUID v4**. The worker auto-generates one on first start and persists it to `/var/lib/gradient-worker/worker-id`:

```sh
cat /var/lib/gradient-worker/worker-id
```

```sh
curl -X POST https://gradient.example.com/api/v1/orgs/myorg/workers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"worker_id": "550e8400-e29b-41d4-a716-446655440001"}'
# → {"error":false,"message":{"peer_id":"<uuid>","token":"<token>"}}
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
    sign  = true;
  };

  settings = {
    maxConcurrentBuilds      = 8;
    evalWorkers              = 2;
    maxConcurrentEvaluations = 2;
    buildMetrics             = true; # opt in to per-build resource metrics for smarter scheduling (enables Nix's cgroups experimental feature)
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

| Option | Default | Description |
|---|---|---|
| `serverUrl` | `null` | WebSocket URL of the server's `/proto` endpoint (required) |
| `workerId` | `null` | Override the worker UUID (`GRADIENT_WORKER_ID`). When null, the ID is read from `$StateDirectory/worker-id` or auto-generated on first start |
| `peersFile` | `null` | Path to peers file (`peer_id:token` per line, `*` = any peer) |
| `useTls` | `true` | Enable TLS (ACME + forceSSL) on the nginx vhost |
| `discoverable` | `false` | Accept incoming connections from the server (reverse-proxy mode) |
| `listenAddr` | `127.0.0.1` | Bind address for the worker listener |
| `port` | `3100` | Listener port when `discoverable` is enabled |
| `capabilities.fetch` | `false` | Prefetch flake inputs |
| `capabilities.eval` | `false` | Run Nix evaluations |
| `capabilities.build` | `false` | Execute Nix builds |
| `capabilities.sign` | `false` | Sign store paths |
| `capabilities.federate` | `false` | Act as a federation relay (requires `discoverable`) |
| `settings.maxConcurrentBuilds` | `100` | Parallel build slots |
| `settings.evalWorkers` | `1` | Number of evaluator subprocesses |
| `settings.maxConcurrentEvaluations` | `1` | Parallel evaluations |
| `settings.cpuCoreScore` | `null` | Override the advertised single-core speed score (higher is faster, `GRADIENT_WORKER_CPU_CORE_SCORE`). When null, the worker benchmarks the host at startup |
| `settings.evalForkWorkers` | `null` | Number of parallel eval subprocesses in the pool (the eval concurrency). When null, auto-sizes to the host core count capped at 16; each worker may hold up to `maxEvalRss` resident (`GRADIENT_EVAL_FORK_WORKERS`) |
| `settings.maxEvalRss` | `2147483648` (2 GiB) | Recycle an eval subprocess (parent-side) once its RSS exceeds this many bytes, so the next acquire spawns a fresh one (`GRADIENT_MAX_EVAL_RSS`) |
| `settings.evalCacheDir` | `null` | Eval-cache directory exported to eval workers as `NIX_CACHE_HOME`. When null, resolves to `{baseDir}/eval-cache` (`GRADIENT_EVAL_CACHE_DIR`) |
| `settings.evalCacheShare` | `true` | Enable fleet eval-cache sharing (pull/push of `<fingerprint>.sqlite` blobs across workers, issue #386) (`GRADIENT_EVAL_CACHE_SHARE`) |
| `settings.maxNixdaemonConnections` | `32` | Worker's local nix-daemon connection pool size. Each in-flight NAR import holds one connection; size for `maxConcurrentBuilds * 8` plus headroom |
| `settings.narPartialTtlSecs` | `86400` (24 h) | TTL in seconds for partially-received NAR downloads (`*.partial`) staged under `<baseDir>/nar-partial`. A periodic sweep deletes partials older than this so an abandoned resumable transfer can't pin disk forever (issue #225). `0` disables the sweep. (`GRADIENT_NAR_PARTIAL_TTL_SECS`) |
| `settings.maxProtoConnections` | `16` | Max simultaneous WebSocket connections (for discoverable mode) |
| `settings.gcrootsDir` | `/nix/var/nix/gcroots/gradient` | Directory for worker-held indirect GC roots. One symlink per active build (drv + outputs) pins inputs and just-built outputs through the daemon so a concurrent `nix-collect-garbage` cannot race the build. Empty string disables |
| `settings.buildMetrics` | `false` | Capture per-build resource metrics (`GRADIENT_WORKER_BUILD_METRICS`) that feed the resource-aware scheduler's RAM/CPU/disk predictions for smarter job placement. Off by default because it turns on Nix's experimental `cgroups` feature + `use-cgroups` on the daemon. CPU time comes from the daemon build result; peak RAM and disk I/O are sampled live from the build's cgroup (located via `buildCgroupStateDir`) — reliable at build concurrency 1, best-effort under concurrency. Wall-clock build time is always reported |
| `settings.buildCgroupRoot` | `/sys/fs/cgroup` | Cgroup-v2 mount root; sampled cgroup paths must live under it (`GRADIENT_WORKER_BUILD_CGROUP_ROOT`) |
| `settings.buildCgroupStateDir` | `/nix/var/nix/cgroups` | Nix's `<state-dir>/cgroups` map of build-user UID → cgroup path; the worker reads the newest entry to locate a running build's cgroup (`GRADIENT_WORKER_BUILD_CGROUP_STATE_DIR`). Granted read access via `ReadOnlyPaths` |
| `settings.logBurstBytesPerMin` | `8388608` (8 MiB) | Burst token bucket: max build-log bytes forwarded to the server per build in any 1-minute window. On trip the worker appends a truncation marker and stops forwarding that build's log (the build still runs). (`GRADIENT_LOG_BURST_BYTES_PER_MIN`) |
| `settings.logSustainedBytesPerHour` | `67108864` (64 MiB) | Sustained token bucket: max build-log bytes forwarded per build in any 1-hour window. (`GRADIENT_LOG_SUSTAINED_BYTES_PER_HOUR`) |
| `settings.logFetchFromStore` | `true` | When a derivation is already built locally (no fresh log), read nix's stored `.bz2` build log and forward it so the UI still shows output. (`GRADIENT_LOG_FETCH_FROM_STORE`) |
| `settings.logLevel.default` | `info` | Worker log level |
| `settings.logLevel.eval` | null | Evaluator log level override |
| `settings.logLevel.build` | null | Builder log level override |
| `settings.logLevel.proto` | null | Protocol log level override |

### Hashing

Gradient hashes NARs and compressed cache files with **SHA-256** by default. No client-side experimental feature is required to substitute from a Gradient cache.

BLAKE3-prefixed (`blake3:{nix32}`) hashes are still accepted on the read path so narinfo rows uploaded while the BLAKE3 default was active (issue #132) keep resolving, and so upstream caches that advertise either algorithm interoperate cleanly.

## Declarative State

Users, organizations, projects, integrations, caches, API keys, custom roles, and workers can be declared in `services.gradient.state` and reconciled on every startup. See [Declarative State](usage/state.md) for the full options reference.

### API keys

State-managed API keys are declared under `state.api_keys.<name>`:

- `key_file` (required, path): file containing the lowercase 64-char SHA-256
  hex digest of the token (without the `GRAD` prefix).
- `owned_by` (required, string): username that owns the key.
- `permissions` (required, list of strings): permission identifiers the key
  grants. See `gradient_db::permissions::Permission` (or
  `GET /user/keys/permissions`) for the full list.
- `organization` (optional, string): organization name to pin the key to.
  Omit for an unscoped key.

### Roles

State-managed custom roles are declared under `state.roles.<name>`:

- `name` (defaults to attrset key): role name. Must be unique within the
  organization and must not collide with built-in role names
  (`Admin`, `Write`, `View`).
- `organization` (required, string): the organization this role belongs to.
- `permissions` (required, list of strings): the capabilities the role grants.

Managed roles cannot be modified or deleted via the API.

### Flake input overrides

Each project may declare per-input flake overrides applied during evaluation fetch. Each entry must set exactly one of:

- `url` - a flake-ref string to replace the input's URL.
- `keep_url = true` - force an update of the input keeping the URL declared in the project's `flake.nix`.

Empty `flake_input_overrides = {}` (the default) means no overrides - `flake.lock` is used as-is. Setting the attrset to `{}` from a non-empty state removes all override rows for that project.

```nix
services.gradient.state.projects.my-project = {
  # ...
  flake_input_overrides = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.keep_url = true;
  };
};
```
