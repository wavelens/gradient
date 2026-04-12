# Architecture

## Overview

Gradient is a multi-crate Rust workspace with two separate binaries: **`gradient`** (the server) and **`gradient-worker`** (the build worker).

### Server

The server binary starts three concurrent async tasks:

```
┌────────────────────────────────────────┐
│           gradient binary              │
│                                        │
│  ┌──────────────────────────────────┐  │
│  │             Builder              │  │
│  │  evaluation queue │ build queue  │  │
│  └──────────────────────────────────┘  │
│  ┌───────────┐   ┌──────────────────┐  │
│  │   Cache   │   │       Web        │  │
│  │   (Axum)  │   │  (Axum /api/v1)  │  │
│  └───────────┘   └──────────────────┘  │
│          ┌───────────────────┐         │
│          │  Proto Scheduler  │         │
│          │  (WebSocket /proto│         │
│          └───────────────────┘         │
└────────────────────────────────────────┘
         │                    │
         └──── PostgreSQL ────┘
```

### Worker

`gradient-worker` is a standalone process that connects to the server over WebSocket at `/proto`. It handles fetch, eval, build, and sign tasks dispatched by the server's scheduler. Workers can run co-located on the server host or on separate machines.

## Crates

```
backend/
├── core/         Shared state, config, DB pool, utility functions
├── entity/       SeaORM entity definitions (one module per table)
├── migration/    SeaORM migrator
├── builder/      Evaluation queue and build status tracking
├── cache/        Nix binary cache server
├── web/          Axum HTTP API
├── proto/        Proto protocol handler and scheduler
└── worker/       gradient-worker binary (fetch, eval, build, sign)
```

### `core`

Defines `ServerState` — the shared `Arc<ServerState>` threaded through every Axum handler and every spawned task. It holds:

- `db: DatabaseConnection` — SeaORM PostgreSQL pool
- `cli: Cli` — resolved configuration from env/flags

Key modules: `core::executer` (Nix store interaction), `core::sources` (key generation, NAR helpers), `core::input` (validation), `core::database` (common queries).

### `entity`

One module per database table using SeaORM derive macros. The naming convention is:

| Alias  | Meaning                    |
|--------|----------------------------|
| `MFoo` | Model (read struct)        |
| `AFoo` | ActiveModel (write struct) |
| `EFoo` | Entity (query entry point) |
| `CFoo` | Column enum                |

Key entities and their relationships:

```
organization
  ├── worker_registration[]    registered worker auth tokens
  ├── cache[]                  binary caches (via subscription)
  ├── derivation[]             immutable per-org "what to build" records
  │     ├── derivation_output[]         (one per Nix output: out, dev, doc, ...)
  │     ├── derivation_dependency[]     (edges: derivation → dependency)
  │     └── derivation_feature[]
  └── project[]
        └── evaluation[]
              ├── commit
              ├── build[]                (one per attempt at a derivation)
              └── entry_point[]          (top-level builds for this eval)
```

The `build` row is the "attempt" — it carries `status`, `log_id`, and `build_time_ms`. Everything immutable about the derivation (path, architecture, outputs, dep graph, required features) lives on `derivation` and is shared across every evaluation that touches it. A rebuild on failure inserts a new `build` row on the same `derivation`.

`derivation_dependency` is a directed edge table: `derivation → dependency` means the `dependency` derivation must be built before `derivation`. The graph is stored once per derivation, not once per evaluation.

`cache_derivation` (cache, derivation) records that a cache holds the **complete closure** of a derivation. The cacher only inserts a row once every output of the derivation is `is_cached = true` AND every transitive dependency already has a matching `cache_derivation` row for the same cache.

`worker_registration` stores `(peer_id, worker_id, token_hash)` — the challenge-response auth tokens issued when a peer (org, cache, or proxy) registers a worker.

### `builder`

Manages the evaluation and build queues. Jobs are dispatched to proto workers via the `Scheduler`. The builder no longer runs builds directly — it enqueues `PendingEvalJob` and `PendingBuildJob` entries that the proto scheduler delivers to connected workers.

See [Internals](internals.md) for algorithm details.

### `proto`

Handles the WebSocket `/proto` endpoint and the scheduler that dispatches jobs to connected workers:

- `handler.rs` — WebSocket lifecycle: handshake, challenge-response auth, capability negotiation, job dispatch loop
- `scheduler/` — `WorkerPool` tracks connected workers; `JobTracker` tracks pending and active jobs; dispatch loops push `JobOffer`s to eligible workers
- `messages/` — rkyv-serialized wire message types (`ServerMessage`, `ClientMessage`)

The scheduler is injected into the Axum router as `Extension<Arc<Scheduler>>` and shared with the builder.

### `worker`

The `gradient-worker` binary. Connects to the server over WebSocket, performs the challenge-response handshake, and executes dispatched jobs:

- `executor/eval.rs` — Nix flake evaluation (spawns evaluator subprocesses)
- `executor/build.rs` — Nix store builds via the local daemon
- `handshake.rs` — client-side challenge-response auth
- `config.rs` — `WorkerConfig` parsed from env vars / CLI args

### `web`

Axum HTTP server. All API routes live under `/api/v1` via `Router::nest`. Auth routes and `/health`/`/config` are outside the authorization middleware layer; everything else passes through `authorization::authorize` which resolves the JWT or API key and injects `Extension<MUser>`.

Endpoints are split by resource in `web/src/endpoints/`:

```
auth.rs          Login, register, OIDC/OAuth2
builds/          Build detail, log streaming, graph, downloads, direct build
caches.rs        Cache CRUD + Nix cache protocol handlers
commits.rs       Commit lookup
evals.rs         Evaluation detail, abort, log streaming
mod.rs           Health, config, 404 handler
orgs/            Org CRUD, members, SSH key, cache subscriptions, worker registration
projects.rs      Project CRUD, entry points, evaluate trigger
user.rs          Profile, API keys, settings
workers.rs       Connected worker list (superuser / global stats)
```

The Nix binary cache endpoints (`/cache/{cache}/…`) are registered at the root router, outside `/api/v1`, to comply with the Nix cache protocol.

## Database

PostgreSQL is the only supported database. Migrations are in `migration/src/` and applied by running `cargo run -p migration`.

All timestamps are `NaiveDateTime` (UTC, stored without timezone). The `NULL_TIME` constant (`1970-01-01 00:00:00`) is used as a sentinel for "never" (e.g. `last_login_at`).

## Frontend

Standalone Angular 21 SPA in `frontend/`. Communicates exclusively with the backend REST API. Built as static files, served by NGINX in production.

Key patterns: standalone components, Angular signals (`signal()`, `computed()`), PrimeNG for UI, SCSS variables from `_variables.scss`.

## CLI

Independent Rust crate in `cli/`. Uses the `connector` sub-crate for typed HTTP calls to the REST API. Auth state is stored in `~/.config/gradient/config`.
