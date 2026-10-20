# Configuration

The `services.gradient` NixOS module exposes one option per setting. The definitions come from `nix/modules`. Each option maps to a CLI flag and an environment variable with the same name.

- `upload.bytesBudget` maps to `--upload-bytes-budget` and `GRADIENT_UPLOAD_BYTES_BUDGET`.
- `worker.build.maxConcurrent` maps to `--build-max-concurrent` and `GRADIENT_WORKER_BUILD_MAX_CONCURRENT`.
- Module-only options such as `packages`, `reverseProxy` and `postgres` lack an environment variable.

Declarative entities under `services.gradient.state` appear in the [state reference](state.md).

--8<-- "docs/generated/configuration-options.md"

## Build Failures

A failed build ends in one of three states.

| Status | Retried | Cause |
|---|---|---|
| `FailedPermanent` | No | The builder exited with a non-zero code, or no connected worker had enough free memory |
| `FailedTransient` | Yes, `build.maxAttempts` times with a doubling `build.retryBackoffSecs` | Out of memory, full disk, network or substitution failure, builder crash |
| `FailedTimeout` | No | `build.defaultTimeoutSecs` or `build.defaultMaxSilentSecs` exceeded |

Derivations can override the server defaults through `timeout`, `maxSilent` and `preferLocalBuild`. The `meta.*` attributes never reach the `.drv`, so they have no effect here.

## Local Worker

Set `localWorker` and `worker.enable` to register the worker on the same host automatically. Registration has three parts.

- A stable worker ID, derived from the host name.
- A token in `/var/lib/gradient-worker/local-token`, generated at first start. Delete the file and restart both services to rotate the token.
- Membership in the state-declared team `server`, which gives every new project access to the worker. Projects created before the team was added get the worker once they add the `server` team.

The worker stays in a reconnect backoff of at most 60 s while no project with a cache subscription is available.

## Postgres Sizing

With `postgres.enable`, host-independent defaults come from `mkDefault` entries.

| Setting | Default | Reason |
|---|---|---|
| `random_page_cost` | `1.1` | Matches SSD storage and keeps the planner on index-only scans of the graph tables |
| `max_connections` | `200` | Covers the three server pools (56 connections by default), autovacuum and `psql` |
| `max_locks_per_transaction` | `1024` | Graph writes lock every shared build they touch. The server logs warnings below `256` |
| `wal_compression` | `lz4` | Shrinks the full page images in the write-ahead log |
| `wal_buffers` | `64MB` | Keeps large graph writes from flushing the write-ahead log mid-transaction |
| `checkpoint_timeout` | `15min` | Fewer checkpoints mean fewer full page images after each one |
| `synchronous_commit` | `off` | A commit no longer waits for the disk. A database crash loses at most the last second of commits, with no corruption |

RAM-dependent settings live in module options.

| Option | Rule of thumb |
|---|---|
| `postgres.sharedBuffers` | A quarter of the RAM, e.g. `"4GB"` on 16 GB |
| `postgres.effectiveCacheSize` | Three quarters of the RAM, e.g. `"12GB"` on 16 GB |
| `postgres.workMem` | `"32MB"` for the default pool sizes |
| `postgres.maintenanceWorkMem` | Up to `"1GB"` with RAM to spare |

All eleven values are manual for an external database (`database.urlFile`).

## Prometheus and OpenTelemetry

The [Monitor Gradient](../guides/monitoring.md) guide covers setup, metric names and alert examples.

## Hashing

Gradient computes SHA-256 hashes for NARs and cached files. Nix clients work without any experimental feature enabled. Uploads made earlier with `blake3:` hashes remain resolvable.
