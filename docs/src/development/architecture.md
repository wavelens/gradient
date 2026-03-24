# Architecture

## Overview

Gradient is a multi-crate Rust workspace. The server binary starts three concurrent async tasks:

```
┌────────────────────────────────────────┐
│           gradient binary              │
│                                        │
│  ┌──────────────────────────────────┐  │
│  │             Builder              │  │
│  │  evaluation loop │ build loop    │  │
│  └──────────────────────────────────┘  │
│  ┌───────────┐   ┌──────────────────┐  │
│  │   Cache   │   │       Web        │  │
│  │   (Axum)  │   │  (Axum /api/v1)  │  │
│  └───────────┘   └──────────────────┘  │
└────────────────────────────────────────┘
         │                    │
         └──── PostgreSQL ────┘
```

## Crates

```
backend/
├── core/        Shared state, config, DB pool, utility functions
├── entity/      SeaORM entity definitions (one module per table)
├── migration/   SeaORM migrator
├── builder/     Evaluator and build scheduler
├── cache/       Nix binary cache server
├── web/         Axum HTTP API
└── nix-daemon/  Nix daemon wire protocol client
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
  ├── server[]           build machines
  ├── cache[]            binary caches (via subscription)
  └── project[]
        └── evaluation[]
              ├── commit
              ├── build[]
              │     ├── build_dependency[] (edges: build → dependency)
              │     └── build_output[]
              └── entry_point[]  (root builds for this evaluation)
```

The `build_dependency` table is a directed edge table: `build → dependency` means the `dependency` derivation must be built before `build`.

### `builder`

Two independent polling loops run concurrently via `tokio::spawn`:

- `schedule_evaluation_loop` — picks up queued evaluations, runs `nix eval`, and populates `build` and `build_dependency` rows.
- `schedule_build_loop` — picks up queued builds, selects a server, copies inputs via SSH, executes the build over the Nix daemon wire protocol, and copies outputs back.

See [Internals](internals.md) for algorithm details.

### `web`

Axum HTTP server. All API routes live under `/api/v1` via `Router::nest`. Auth routes and `/health`/`/config` are outside the authorization middleware layer; everything else passes through `authorization::authorize` which resolves the JWT or API key and injects `Extension<MUser>`.

Endpoints are split by resource in `web/src/endpoints/`:

```
auth.rs      Login, register, OIDC/OAuth2
builds.rs    Build detail, log streaming, graph, downloads, direct build
caches.rs    Cache CRUD + Nix cache protocol handlers
commits.rs   Commit lookup
evals.rs     Evaluation detail, abort, log streaming
mod.rs       Health, config, 404 handler
orgs.rs      Org CRUD, members, SSH key, cache subscriptions
projects.rs  Project CRUD, entry points, evaluate trigger
servers.rs   Server CRUD, connection test, active toggle
user.rs      Profile, API keys, settings
```

The Nix binary cache endpoints (`/cache/{cache}/…`) are registered at the root router, outside `/api/v1`, to comply with the Nix cache protocol.

### `nix-daemon`

A hand-written implementation of the Nix daemon binary protocol (the same protocol used by `nix-store --daemon`). Gradient uses it to:

1. Query path info and missing paths on the local Nix store (during evaluation).
2. Send `AddToStoreNar` and `BuildDerivation` commands to remote build servers over SSH.

The crate supports both Unix socket connections (local daemon) and command-duplex connections (subprocess stdin/stdout, used when a socket is unavailable). The two variants are unified under `LocalNixStore`.

## Database

PostgreSQL is the only supported database. Migrations are in `migration/src/` and applied by running `cargo run -p migration`.

All timestamps are `NaiveDateTime` (UTC, stored without timezone). The `NULL_TIME` constant (`1970-01-01 00:00:00`) is used as a sentinel for "never" (e.g. `last_login_at`).

## Frontend

Standalone Angular 21 SPA in `frontend/`. Communicates exclusively with the backend REST API. Built as static files, served by NGINX in production.

Key patterns: standalone components, Angular signals (`signal()`, `computed()`), PrimeNG for UI, SCSS variables from `_variables.scss`.

## CLI

Independent Rust crate in `cli/`. Uses the `connector` sub-crate for typed HTTP calls to the REST API. Auth state is stored in `~/.config/gradient/config`.
