# Architecture

Gradient is a Rust server, a Rust worker, an Angular frontend and a Rust CLI. The database and the NAR store belong to the server. The server has no connection to a Nix daemon. Workers perform every store operation.

```mermaid
flowchart LR
    ui[Frontend] -->|/api/v1| web
    cli[gradient CLI] -->|/api/v1| web
    nix[nix clients] -->|/cache| web
    subgraph server[gradient-server]
        web[web] --> actors[graph, scheduler, effects]
        proto[proto sessions] --> actors
    end
    worker[gradient-worker] <-->|/proto| proto
    actors --> pg[(PostgreSQL)]
    actors --> store[("NAR store, file or S3")]
    worker --> daemon[nix-daemon]
```

## Binaries

| Binary | Crate | Role |
|---|---|---|
| `gradient-server` | `backend/` (root) | HTTP API, binary cache, worker sessions and the scheduler |
| `gradient-worker` | `gradient-worker` | Fetch, evaluate and build through a local `nix-daemon` |
| `gradient` | `cli/` | Command-line client for the REST API |
| `gradient-sql-gate` | `backend/` (feature `sql-gate`) | Explain every registered statement, see [Tests](tests.md#sql-plan-check) |
| `gradient-daemon` | `gradient-daemon` (feature `mock`) | Nix daemon protocol server with a mock store, built for VM tests |
| `gradient-proxy` | Separate, closed-source repository | Gateway for [Federation](proto/federation.md) |

## Server

One process makes up the server. All long-lived tasks live under a supervision tree (`gradient_util::supervision`, built on ractor), rooted at the shared `Shutdown` coordinator. Subsystems can register children through `Shutdown::supervise` or `supervise_now`.

```text
root
├── graph                                actor: sole writer of the graph and the cache index; stopped last
├── scheduler                            supervisor
│   ├── scheduler-core                   actor: WorkerPool and JobTracker behind messages
│   ├── trigger-dispatch                 every 5 s
│   ├── eval-dispatch                    5 s tick, woken by every created evaluation
│   ├── build-dispatch                   actor: 5 s tick, kicks, startable-set resync every 60 s
│   ├── transient-retry-requeue          every 5 s: transient failures past their backoff back to the queue
│   ├── cluster-dispatch                 5 s tick, woken when a worker finds no single job
│   └── upstream-probe                   1 s tick, woken by each probe request: upstream cache lookups for newly wanted shared builds and their dependencies
├── sessions                             supervisor: one actor per worker connection
├── worker-sample, instance-metrics      metrics collection passes
├── unreclaimed-job-sweep                every 10 s: requeue jobs of disconnected workers that outlived the reclaim grace
├── worker-liveness, graph-consistency,
│   graph-stuck-reheal                   absent when disabled
├── eval-completion-watchdog,
│   stranded-build-sweep,
│   abandoned-dispatch-sweep             every 60 s
├── cache-maintenance, sign-sweep,
│   debug-index, eval-cache-sweep        cache sweeps
├── effects                              actor: sends pending deliveries; effects-workers factory beneath
├── retention (hourly), rollup,
│   cache_metric_flush, otlp-snapshot    metrics pipeline; otlp-snapshot only with an OTLP endpoint
└── outbound-connect                     every 15 s: dials workers with a registered URL
```

| Actor | Owned State | Details |
|---|---|---|
| `graph` | Every request-path write to `derivation*`, `build_job`, `build_attempt`, `cached_path`, and the retires of maintenance | [Shared Builds](scheduler/shared-builds.md) |
| `scheduler-core` | `WorkerPool` and the candidate cache (`JobTracker`). `Scheduler` is a facade, one message per method | [Capabilities and Assignment](proto/capabilities-and-dispatch.md) |
| `SessionActor` | One worker connection. Frames come from the reader in order, up to 32 MiB unanswered. TCP backpressure can hold any frame past that limit. `CacheQuery` and `QueryKnownDerivations` get their answer from the reader directly. | [Connection](proto/connection.md) |
| `effects` | Pending deliveries (`pending_delivery`): 8 workers, 6 attempts with backoff doubling from 30 s (capped at 15 min), then dead letter | [Events and Webhooks](../reference/events.md) |

- **Pull-based assignment:** A claim means a `dispatched_job` insert in Postgres (`gradient_db::scheduling::assignment_record::claim_assignment`). The scheduler actor can only cache candidates.
- **Session signals:** The scheduler can reach a session only through `SessionPort` (`Offers`, `Reauth`, `Abort`, `Close`). A burst of enqueues will collapse into one offer per generation.
- **Off-session RPCs:** The reader will run `CacheQuery` and `QueryKnownDerivations` as tracked tasks the moment they arrive. `WorkerMetrics` will run the same way inside the session. A slow frame can never hold back a lookup the worker needs. Log chunks go through a per-session lane, flushed before a job's completion. A respawned core actor will get every live session and its jobs back from the sessions supervisor.
- **State:** `AppState` (alias `ServerState`) contains three pools (`worker_db`, `web_db`, `cache_db`), `RuntimeConfig` and the NAR store. Also part of the state: `UploadAdmission`, the graph handle, the event bus, and the `startable_set` and `probe_requests` channels for build-dispatch and the probe.
- **Events:** Each event is a typed value (`gradient_types::events::Event`) and can travel two ways. The in-process `EventBus` will feed live sockets and `/api/v1/metrics/events`, with a slow subscriber skipping. `gradient_db::deliveries::events::record` will store durable pending deliveries (`pending_delivery` rows) inside the caller's transaction. The `effects` actor will spread those rows out into action and webhook deliveries.
- **Uploads:** Admission happens before any byte can move. All uploads share one server-wide count and byte budget, round-robin across sessions, FIFO within one, small uploads first. See [Transfer](proto/transfer.md#upload) and [NAR Storage](internals/nar-storage.md).

### Supervision Rules

- A panicking or exiting child will come back after a backoff of 1 s doubling to 60 s. Five healthy minutes reset the backoff.
- A pass over its budget will be cancelled in place and will tick again afterwards.
- Restarts, errors, timeouts and the last good pass per child appear on the `/api/v1/board/health` endpoint.
- Run work outliving a request as a tracked task. Shutdown will drain these tasks. Bare `tokio::spawn` is a clippy error in the backend workspace.

## Worker

- Connecting to the server at `/proto`, or accepting connections with the `discoverable` option.
- Running flake jobs (fetch, evaluate) and build jobs ([Jobs](proto/jobs.md)).
- Evaluating in a subprocess pool ([Eval Worker Setup](eval-worker.md)).
- Talking to the local `nix-daemon` through harmonia and holding GC roots for the worker's builds.
- Never signing. Signatures for every cached path come from the server ([Cache Serving](internals/cache-serving.md)).

## Crates

The workspace root is `gradient-server`. All backend crates live under `backend/gradient-<name>`.

| Group | Crate | Role |
|---|---|---|
| Server state | `core` | `AppState`, upstream narinfo lookup, sources |
| | `db` | All queries plus a graph repair pass over the pools, and `DbContext` |
| | `entity` | SeaORM entities, one module per table |
| | `migration` | SeaORM migrator |
| | `graph` | Graph writer |
| | `effects` | Effects actor |
| Scheduling | `scheduler` | Scheduler actor, assignment passes, upstream probe |
| | `pool` | Worker registry, capability aggregate, scoring rules |
| Protocol | `wire` | Protocol types, framing, handshake, dial and accept |
| | `proto` | Server side: sessions, NAR transfer, signing, assignment handlers |
| | `worker-client` | Peer side: connection, reconnect, reply correlation. Shared by worker and proxy |
| HTTP | `web` | Axum API and binary cache endpoints |
| | `git-host` | Reporters per Git host, webhook parsing, signature checks |
| Background | `cache` | Cache sweeps: maintenance, signing backfill, debug index, eval cache, deep GC |
| | `ci` | Triggers, `apply_trigger`, evaluation creation, Git host checks |
| | `state` | Declarative state DTOs and apply |
| | `notify` | Email through `EmailSender` |
| | `report` | Diagnostic report extractor |
| Storage and Nix | `storage` | NAR and log storage (file or S3), upload admission, partial transfers, passthrough, hot cache |
| | `sources` | Store paths, the daemon pool, git and SSH sources, cache keys, the `flake.lock` updater |
| | `derivation` | `.drv` parsing |
| | `eval` | Flake evaluator, used by the worker and `gradient eval` |
| Binaries | `worker` | `gradient-worker` |
| | `daemon` | Mock Nix daemon for VM tests |
| Shared | `types` | IDs, runtime config, events, entity aliases |
| | `util` | Shutdown, supervision, HTTP clients, logging, metrics setup |
| | `test-support` | Shared test fixtures, see [Tests](tests.md#test-support) |

**Entity aliases** (`gradient-types/src/entity_aliases.rs`): `MFoo` model, `AFoo` active model, `EFoo` entity, `CFoo` column.

## Database

- PostgreSQL 18 or newer serves as the sole database. Migrations live in `backend/gradient-migration/src/` and the server applies them at startup ([Migrations](migrations.md)).
- Deadlocks and serialization failures trigger up to three retries for graph transactions. Passes outside the graph writer get no retry and repeat on their next tick.
- Counter seeds and state changes stay consistent through advisory locks in namespace `643` (`gradient_db::graph::shared_build_guard`) plus hash-ordered row locks ([Shared Builds](scheduler/shared-builds.md)).
- A `max_locks_per_transaction` value of at least 256 is required. The server logs an error below that threshold.
- Timestamps use `NaiveDateTime` in UTC. The `NULL_TIME` value (`1970-01-01 00:00:00`) can stand for "never".
- `backend/clippy.toml` forbids `reqwest::Client::new` and raw `Statement` construction. All statements go through `gradient_db::sql!` for the plan check.

## Frontend

- `frontend/` holds a standalone Angular 22 app. It communicates only with the REST API. The build output is static files, served by nginx or Caddy.
- Components are standalone and use signals. Styling relies on SCSS variables from `_variables.scss`. Charts come from Apache ECharts, and the in-repo `gr-ui` layer builds on `@angular/cdk`. Details in the [Frontend Style Guide](frontend-style-guide.md).

## CLI

- The `cli/` workspace contains two crates: `gradient-cli`, the binary named `gradient`, and `connector`, a typed client for the REST API.
- The optional `eval` feature points at `backend/gradient-eval` as a path dependency, so local evaluation work against that crate.
- Settings live in `$XDG_CONFIG_HOME/gradient/config.toml`, which resolves to `~/.config/gradient/config.toml`. On macOS, an existing legacy native path takes precedence and stays in use.

## Related

- [Internals](internals/index.md): webhook handling, NAR storage, cache serving, graph queries, and authentication for the Git host
- [Scheduler](scheduler/index.md) and [Proto](proto/index.md)
