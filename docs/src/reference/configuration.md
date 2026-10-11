# Configuration

Every option of the `services.gradient` NixOS module, generated from `nix/modules`. Each option will map onto a flag and an environment variable of the same name.

- `upload.bytesBudget` is `--upload-bytes-budget` and `GRADIENT_UPLOAD_BYTES_BUDGET`.
- `worker.build.maxConcurrent` is `--build-max-concurrent` and `GRADIENT_WORKER_BUILD_MAX_CONCURRENT`.
- Module-only options (`packages`, `reverseProxy`, `postgres`, ...) have no environment variable.

Declarative entities under `services.gradient.state` are in the [state reference](state.md).

--8<-- "docs/generated/configuration-options.md"

## Build Failures

| Status | Retried | Cause |
|---|---|---|
| `FailedPermanent` | No | The builder exited non-zero, or no connected worker had enough free memory |
| `FailedTransient` | Yes, `build.maxAttempts` times with a doubling `build.retryBackoffSecs` | Out of memory, full disk, network or substitution failure, builder crash |
| `FailedTimeout` | No | `build.defaultTimeoutSecs` or `build.defaultMaxSilentSecs` exceeded |

`timeout`, `maxSilent` and `preferLocalBuild` on a derivation override the server defaults. `meta.*` attributes never reach the `.drv` and have no effect here.

## Local Worker

`localWorker` and `worker.enable` together register the worker of the same host without manual steps.

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
