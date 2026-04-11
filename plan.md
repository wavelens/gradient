# Plan: Server/Worker Separation & Codebase Restructuring

## Goal

Split Gradient into two distinct binaries — **gradient-server** and **gradient-worker** — with clear
responsibilities. The server owns the database, API, cache, and job coordination. Workers own Nix
evaluation, building, compression, and signing. All communication between them flows through the
`/proto` WebSocket. The codebase is restructured around named structs with descriptive method calls.
Testing stays fully mocked.

---

## Current State (Problems)

1. **Single monolith binary** runs evaluator, builder, cache, and web in one process. The server
   directly SSH-es into "server" entities to build derivations — there is no real worker separation.

2. **`ServerState` is a god object** — holds 11 trait objects and the CLI config. Every function
   takes `Arc<ServerState>` and pulls out what it needs. Hard to test in isolation, hard to reason
   about what a function actually depends on.

3. **Long tuple return types** — `evaluate()` returns an 8-element tuple (`EvaluationOutput`).
   Functions pass unnamed positional data around making debugging painful.

4. **Database queries scattered everywhere** — raw SeaORM queries in scheduler loops, eval code,
   build code, cache code. No repository/service layer. Business logic is tangled with persistence.

5. **`server` entity is a build machine** — the name collides with the actual Gradient server. This
   entity disappears entirely when builds move to workers.

6. **`SshBuildExecutor`** becomes unnecessary — workers build locally via their nix-daemon, not over
   SSH tunnels.

7. **Implicit state transitions** — `BuildStatus` and `EvaluationStatus` transitions happen in
   scattered `update_build_status` calls. No state machine enforces valid transitions.

8. **No worker binary exists** — `src/bin/worker.rs` is a stub with CLI flags but no runtime.

---

## Target Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      gradient-server                        │
│                                                             │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌───────────┐  │
│  │   Web    │  │  Cache   │  │Scheduler │  │  Proto    │  │
│  │  (API)   │  │ (NAR+GC) │  │(dispatch)│  │(WebSocket)│  │
│  └──────────┘  └──────────┘  └──────────┘  └───────────┘  │
│       │              │             │              │         │
│  ┌─────────────────────────────────────────────────────┐   │
│  │                   Database Layer                     │   │
│  │  (repositories: EvalRepo, BuildRepo, DerivRepo,     │   │
│  │   CacheRepo, OrgRepo, ProjectRepo, ...)             │   │
│  └─────────────────────────────────────────────────────┘   │
│       │                                      ▲             │
│       ▼                                      │             │
│  ┌──────────┐                          ┌───────────┐       │
│  │PostgreSQL│                          │ NarStore  │       │
│  └──────────┘                          └───────────┘       │
└──────────────────────────────────────┬──────────────────────┘
                                       │ /proto WebSocket
                ┌──────────────────────┼──────────────────────┐
                │                      │                      │
        ┌───────▼──────┐       ┌───────▼──────┐       ┌──────▼───────┐
        │   Worker A   │       │   Worker B   │       │   Worker C   │
        │ fetch+eval   │       │ build+sign   │       │ build+comp   │
        └──────────────┘       └──────────────┘       └──────────────┘
```

**Server capabilities:** cache serving, job scheduling, NAR storage, API, CI reporting, webhooks.
**Worker capabilities:** fetch, eval, build, compress, sign (any combination).

---

## Phase 0: Preparation (Non-Breaking)

### 0.1 — Introduce repository pattern for database access

**Why:** Every service currently does raw SeaORM queries inline. Before splitting, we need a clean
data-access boundary so the server can own all DB access and workers never touch the database.

Create `backend/core/src/repo/` with one struct per aggregate root:

```rust
// core/src/repo/eval.rs
pub struct EvalRepo<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> EvalRepo<'db> {
    pub fn new(db: &'db DatabaseConnection) -> Self { ... }

    pub async fn find_next_queued(&self) -> Result<Option<MEvaluation>> { ... }
    pub async fn update_status(&self, id: Uuid, status: EvaluationStatus) -> Result<()> { ... }
    pub async fn update_status_with_error(&self, id: Uuid, status: EvaluationStatus, error: String) -> Result<()> { ... }
    pub async fn mark_completed(&self, id: Uuid) -> Result<()> { ... }
    pub async fn insert_messages(&self, eval_id: Uuid, messages: Vec<EvalMessage>) -> Result<()> { ... }
}
```

```rust
// core/src/repo/build.rs
pub struct BuildRepo<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> BuildRepo<'db> {
    pub fn new(db: &'db DatabaseConnection) -> Self { ... }

    pub async fn find_next_queued(&self, skip: &HashSet<Uuid>) -> Result<Option<(MBuild, MDerivation)>> { ... }
    pub async fn update_status(&self, id: Uuid, status: BuildStatus) -> Result<()> { ... }
    pub async fn cascade_dependency_failed(&self, id: Uuid) -> Result<Vec<Uuid>> { ... }
    pub async fn insert_builds(&self, builds: Vec<MBuild>) -> Result<()> { ... }
    pub async fn record_build_time(&self, id: Uuid, elapsed: Duration) -> Result<()> { ... }
}
```

```rust
// core/src/repo/derivation.rs
pub struct DerivationRepo<'db> { ... }

impl<'db> DerivationRepo<'db> {
    pub async fn insert_derivations(&self, derivations: Vec<MDerivation>) -> Result<()> { ... }
    pub async fn insert_outputs(&self, outputs: Vec<ADerivationOutput>) -> Result<()> { ... }
    pub async fn insert_dependencies(&self, deps: Vec<MDerivationDependency>) -> Result<()> { ... }
    pub async fn find_uncached_outputs(&self, limit: usize) -> Result<Vec<MDerivationOutput>> { ... }
    pub async fn mark_output_cached(&self, id: Uuid) -> Result<()> { ... }
    pub async fn gc_orphans(&self, grace_hours: i64) -> Result<u64> { ... }
}
```

Also: `CacheRepo`, `OrgRepo`, `ProjectRepo`, `ServerRepo` (renamed to `WorkerRepo` later),
`UserRepo`, `WebhookRepo`, `EntryPointRepo`.

**Files to create:**
- `core/src/repo/mod.rs`
- `core/src/repo/eval.rs`
- `core/src/repo/build.rs`
- `core/src/repo/derivation.rs`
- `core/src/repo/cache.rs`
- `core/src/repo/org.rs`
- `core/src/repo/project.rs`
- `core/src/repo/user.rs`
- `core/src/repo/webhook.rs`
- `core/src/repo/entry_point.rs`

**Migration path:** Extract existing inline queries one file at a time. Each repo function replaces
one or more raw query sites. Tests validate the extraction produces identical SQL.

### 0.2 — Replace tuple returns with named structs

**Why:** `EvaluationOutput` is a type alias for an 8-element tuple. This is unreadable and
error-prone.

```rust
// evaluator/src/eval.rs — BEFORE
pub type EvaluationOutput = (
    Vec<MBuild>,
    Vec<MDerivation>,
    Vec<ADerivationOutput>,
    Vec<MDerivationDependency>,
    Vec<(Uuid, String)>,
    Vec<(String, String)>,
    Vec<(Uuid, Vec<String>)>,
    Vec<String>,
);

// AFTER
pub struct EvaluationResult {
    pub builds: Vec<MBuild>,
    pub derivations: Vec<MDerivation>,
    pub derivation_outputs: Vec<ADerivationOutput>,
    pub derivation_dependencies: Vec<MDerivationDependency>,
    pub entry_points: Vec<EntryPoint>,
    pub failed_derivations: Vec<FailedDerivation>,
    pub pending_features: Vec<PendingFeature>,
    pub warnings: Vec<String>,
}

pub struct EntryPoint {
    pub build_id: Uuid,
    pub wildcard: String,
}

pub struct FailedDerivation {
    pub derivation: String,
    pub error: String,
}

pub struct PendingFeature {
    pub derivation_id: Uuid,
    pub features: Vec<String>,
}
```

Similarly for `BuildExecutionResult` (already a struct but `error_msg: String` should become
`Result<Vec<ExecutedBuildOutput>, BuildError>`).

### 0.3 — Explicit state machines for status transitions

**Why:** Status transitions are scattered across the codebase with no enforcement. A bug can
transition `Completed → Building`.

```rust
// core/src/state_machine/build.rs
pub struct BuildStateMachine;

impl BuildStateMachine {
    pub fn transition(current: BuildStatus, target: BuildStatus) -> Result<BuildStatus, InvalidTransition> {
        match (current, target) {
            (Created, Queued) => Ok(Queued),
            (Queued, Building) => Ok(Building),
            (Building, Completed) => Ok(Completed),
            (Building, Failed) => Ok(Failed),
            (_, Aborted) => Ok(Aborted),
            (_, DependencyFailed) => Ok(DependencyFailed),
            _ => Err(InvalidTransition { from: current, to: target }),
        }
    }
}
```

`BuildRepo::update_status` calls `BuildStateMachine::transition` internally. Invalid transitions
become hard errors in dev, logged warnings in prod.

### 0.4 — Rename `server` entity to `build_machine` (DB migration)

**Why:** The `server` table represents remote build machines (host, port, SSH username). This
conflicts with the Gradient server concept. In the new architecture, this entity is replaced by
proto-connected workers — but we rename it first to reduce confusion during the transition.

- New migration: `ALTER TABLE server RENAME TO build_machine;`
- Rename entity: `entity/src/server.rs` → `entity/src/build_machine.rs`
- Rename all `MServer`, `EServer`, `AServer`, `CServer` → `MBuildMachine`, etc.
- Rename API endpoints: `/servers/{org}/{server}` → `/build-machines/{org}/{name}` (keep old
  routes as 301 redirects for one release)

This entity will eventually be removed entirely (Phase 3) when all builds go through workers.

---

## Phase 1: Extract Worker Binary

### 1.1 — Create `backend/worker/` crate

New workspace member: `backend/worker/`. This crate contains the worker runtime — it connects to
a Gradient server via WebSocket and executes jobs locally.

```
worker/
├── Cargo.toml
├── src/
│   ├── main.rs              # CLI parsing, connect, main loop
│   ├── connection.rs         # WebSocket client, reconnect logic
│   ├── handshake.rs          # InitConnection, capability negotiation
│   ├── executor/
│   │   ├── mod.rs
│   │   ├── fetch.rs          # FetchFlake — clone repo via git2
│   │   ├── eval.rs           # EvaluateFlake + EvaluateDerivations — delegate to eval workers
│   │   ├── build.rs          # Build — local nix-daemon build
│   │   ├── compress.rs       # Compress — zstd NAR packing
│   │   └── sign.rs           # Sign — Ed25519 signing
│   ├── job.rs                # Job execution orchestrator (run task chain)
│   ├── scorer.rs             # Local store scoring for JobOffer candidates
│   ├── nar.rs                # NAR transfer (direct + S3)
│   └── store.rs              # Local NixStoreProvider wrapper
```

**Key struct:**

```rust
pub struct Worker {
    config: WorkerConfig,
    capabilities: GradientCapabilities,
    nix_store: LocalNixStoreProvider,
    eval_pool: Option<EvalWorkerPool>,    // only if eval capability
    scorer: JobScorer,
    connection: ProtoConnection,
}

impl Worker {
    pub async fn connect(config: WorkerConfig) -> Result<Self> { ... }
    pub async fn run(&mut self) -> Result<()> { ... }     // main loop
}
```

**Job executor:**

```rust
pub struct JobExecutor {
    nix_store: Arc<LocalNixStoreProvider>,
    eval_pool: Option<Arc<EvalWorkerPool>>,
}

impl JobExecutor {
    pub async fn execute_flake_job(&self, job: FlakeJob, tx: &mut JobUpdater) -> Result<()> {
        for task in &job.tasks {
            match task {
                FlakeTask::FetchFlake => self.fetch_repository(&job, tx).await?,
                FlakeTask::EvaluateFlake => self.evaluate_flake(&job, tx).await?,
                FlakeTask::EvaluateDerivations => self.evaluate_derivations(&job, tx).await?,
            }
        }
        Ok(())
    }

    pub async fn execute_build_job(&self, job: BuildJob, tx: &mut JobUpdater) -> Result<()> {
        for build in &job.builds {
            self.build_derivation(build, tx).await?;
        }
        if let Some(compress) = &job.compress {
            self.compress_outputs(compress, tx).await?;
        }
        if let Some(sign) = &job.sign {
            self.sign_outputs(sign, tx).await?;
        }
        Ok(())
    }

    async fn fetch_repository(&self, job: &FlakeJob, tx: &mut JobUpdater) -> Result<()> { ... }
    async fn evaluate_flake(&self, job: &FlakeJob, tx: &mut JobUpdater) -> Result<()> { ... }
    async fn evaluate_derivations(&self, job: &FlakeJob, tx: &mut JobUpdater) -> Result<()> { ... }
    async fn build_derivation(&self, build: &BuildTask, tx: &mut JobUpdater) -> Result<()> { ... }
    async fn compress_outputs(&self, task: &CompressTask, tx: &mut JobUpdater) -> Result<()> { ... }
    async fn sign_outputs(&self, task: &SignTask, tx: &mut JobUpdater) -> Result<()> { ... }
}
```

**`JobUpdater`** wraps the WebSocket sender and provides typed methods:

```rust
pub struct JobUpdater<'a> {
    job_id: String,
    socket: &'a mut ProtoConnection,
}

impl<'a> JobUpdater<'a> {
    pub async fn report_fetching(&mut self) -> Result<()> { ... }
    pub async fn report_evaluating_flake(&mut self) -> Result<()> { ... }
    pub async fn report_eval_result(&mut self, derivations: Vec<DiscoveredDerivation>, warnings: Vec<String>) -> Result<()> { ... }
    pub async fn report_building(&mut self, build_id: &str) -> Result<()> { ... }
    pub async fn report_build_output(&mut self, build_id: &str, outputs: Vec<BuildOutput>) -> Result<()> { ... }
    pub async fn report_compressing(&mut self) -> Result<()> { ... }
    pub async fn report_signing(&mut self) -> Result<()> { ... }
    pub async fn send_log_chunk(&mut self, task_index: u32, data: Vec<u8>) -> Result<()> { ... }
    pub async fn complete(&mut self) -> Result<()> { ... }
    pub async fn fail(&mut self, error: String) -> Result<()> { ... }
}
```

### 1.2 — Move evaluation logic from `evaluator/` to `worker/`

The evaluator crate currently does two things:
1. **Scheduler** (server-side): picks evaluations from DB, spawns tasks, inserts results
2. **Evaluation** (worker-side): prefetch, nix eval, closure walk

Split them:
- `evaluator/src/scheduler/` stays on the server (becomes thinner — just creates FlakeJobs and
  dispatches via proto)
- `evaluator/src/eval.rs`, `evaluator/src/flake.rs`, `evaluator/src/dependencies.rs` → move to
  `worker/src/executor/eval.rs`
- `evaluator/src/worker.rs`, `evaluator/src/worker_pool/` → move to `worker/src/executor/eval.rs`
  (the eval worker subprocess pool runs inside the worker binary, not the server)
- `evaluator/src/nix_eval.rs` → stays in evaluator crate (shared between worker subprocess and
  test mocks)

### 1.3 — Move build execution to `worker/`

- `core/src/executer/ssh.rs` (`SshBuildExecutor`) — **delete entirely**. Workers build locally via
  their own nix-daemon. No more SSH tunneling.
- `core/src/executer/mod.rs` (`BuildExecutor` trait) — **delete**. Replace with
  `worker/src/executor/build.rs` which calls the local nix-daemon directly:

```rust
// worker/src/executor/build.rs
pub struct LocalBuilder {
    nix_store: Arc<LocalNixStoreProvider>,
}

impl LocalBuilder {
    /// Build a single derivation on the local nix-daemon.
    pub async fn build_derivation(&self, task: &BuildTask, tx: &mut JobUpdater) -> Result<Vec<BuildOutput>> {
        tx.report_building(&task.build_id).await?;

        let daemon = self.nix_store.connect().await?;
        let drv = daemon.read_derivation(&task.drv_path).await?;
        let result = daemon.build_derivation(&task.drv_path, &drv).await?;

        let outputs = self.collect_build_outputs(&result).await?;
        tx.report_build_output(&task.build_id, outputs.clone()).await?;

        Ok(outputs)
    }

    async fn collect_build_outputs(&self, result: &BuildResult) -> Result<Vec<BuildOutput>> { ... }
}
```

### 1.4 — Move signing and compression to `worker/`

- `cache/src/cacher/signing.rs` — signing logic moves to `worker/src/executor/sign.rs`
- New: `worker/src/executor/compress.rs` — NAR compression using zstd

```rust
// worker/src/executor/compress.rs
pub struct NarCompressor {
    nix_store: Arc<LocalNixStoreProvider>,
}

impl NarCompressor {
    /// Compress store paths into zstd NARs for upload.
    pub async fn compress_outputs(&self, task: &CompressTask, tx: &mut JobUpdater) -> Result<Vec<CompressedNar>> {
        tx.report_compressing().await?;
        let mut results = Vec::new();
        for path in &task.store_paths {
            let nar = self.dump_and_compress(path).await?;
            results.push(nar);
        }
        Ok(results)
    }

    async fn dump_and_compress(&self, store_path: &str) -> Result<CompressedNar> { ... }
}
```

### 1.5 — Worker NixOS module already done

`nix/modules/gradient-worker.nix` is already written as the many-to-many attrset module. Each
`services.gradient.workers.<name>` instance gets its own systemd service, state directory, and
env config.

---

## Phase 2: Server-Side Restructuring

### 2.1 — New `scheduler/` crate (replace `builder/` + `evaluator/scheduler/`)

The server no longer runs evaluations or builds directly. Instead, it creates jobs and dispatches
them to workers via the proto WebSocket. Create `backend/scheduler/`:

```
scheduler/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── eval_scheduler.rs     # Create FlakeJobs from queued evaluations
│   ├── build_scheduler.rs    # Create BuildJobs from queued builds
│   ├── dispatcher.rs         # Match jobs to workers, handle scores
│   ├── worker_pool.rs        # Track connected workers + their capabilities
│   └── state_machine.rs      # Build/eval status transitions
```

**Key structs:**

```rust
// scheduler/src/worker_pool.rs
pub struct ConnectedWorker {
    pub id: String,
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<Architecture>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_jobs: HashSet<String>,
    pub draining: bool,
}

pub struct WorkerPool {
    workers: HashMap<String, ConnectedWorker>,
}

impl WorkerPool {
    pub fn register_worker(&mut self, id: String, caps: GradientCapabilities) { ... }
    pub fn update_capabilities(&mut self, id: &str, caps: WorkerCapabilities) { ... }
    pub fn remove_worker(&mut self, id: &str) -> Vec<String> { /* returns orphaned job_ids */ ... }
    pub fn find_eval_workers(&self) -> Vec<&ConnectedWorker> { ... }
    pub fn find_build_workers(&self, arch: Architecture, features: &[String]) -> Vec<&ConnectedWorker> { ... }
    pub fn mark_draining(&mut self, id: &str) { ... }
}
```

```rust
// scheduler/src/eval_scheduler.rs
pub struct EvalScheduler {
    eval_repo: EvalRepo,
    worker_pool: Arc<RwLock<WorkerPool>>,
}

impl EvalScheduler {
    /// Poll for queued evaluations and create FlakeJobs for eligible workers.
    pub async fn create_flake_job(&self, evaluation: &MEvaluation) -> Result<(String, FlakeJob)> { ... }

    /// Process an EvalResult batch from a worker: insert derivations, create builds.
    pub async fn handle_eval_result(&self, job_id: &str, derivations: Vec<DiscoveredDerivation>, warnings: Vec<String>) -> Result<()> { ... }
}
```

```rust
// scheduler/src/build_scheduler.rs
pub struct BuildScheduler {
    build_repo: BuildRepo,
    derivation_repo: DerivationRepo,
    worker_pool: Arc<RwLock<WorkerPool>>,
}

impl BuildScheduler {
    /// Create a BuildJob for the given build (with full dependency chain).
    pub async fn create_build_job(&self, build: &MBuild) -> Result<(String, BuildJob)> { ... }

    /// Process a BuildOutput from a worker: update derivation_output rows.
    pub async fn handle_build_output(&self, build_id: &str, outputs: Vec<BuildOutput>) -> Result<()> { ... }

    /// Handle job completion: update status, check evaluation, cascade.
    pub async fn handle_job_completed(&self, job_id: &str) -> Result<()> { ... }

    /// Handle job failure: mark failed, cascade DependencyFailed.
    pub async fn handle_job_failed(&self, job_id: &str, error: &str) -> Result<()> { ... }
}
```

### 2.2 — Implement proto handler business logic

Fill in all the TODO stubs in `proto/src/handler.rs` using the new scheduler:

```rust
// proto/src/handler.rs — the dispatch loop calls into scheduler
ClientMessage::WorkerCapabilities { architectures, system_features, max_concurrent_builds } => {
    scheduler.worker_pool.write().await.update_capabilities(
        &peer_id, WorkerCapabilities { architectures, system_features, max_concurrent_builds }
    );
}

ClientMessage::RequestJobChunk { scores, is_final } => {
    scheduler.dispatcher.receive_scores(&peer_id, scores, is_final).await;
}

ClientMessage::JobUpdate { job_id, update } => {
    match update {
        JobUpdateKind::EvalResult { derivations, warnings } => {
            scheduler.eval_scheduler.handle_eval_result(&job_id, derivations, warnings).await?;
        }
        JobUpdateKind::BuildOutput { build_id, outputs } => {
            scheduler.build_scheduler.handle_build_output(&build_id, outputs).await?;
        }
        // ... other kinds are status updates → repo calls
    }
}
```

### 2.3 — Slim down `core/` crate

After the split, `core/` should contain only:
- `repo/` — database repositories
- `types/` — shared types, CLI structs, entity aliases
- `storage/` — `LogStorage`, `NarStore`, `EmailSender`
- `ci/` — CI reporter, webhook client
- `state_machine/` — status transition logic
- `nix/` — `NixFlakeUrl`, `Wildcard` (URL/pattern utilities, NOT evaluation)

**Remove from core:**
- `core/src/executer/` — `BuildExecutor` trait and `SshBuildExecutor` (replaced by worker-local builds)
- `core/src/executer/pool.rs` — `NixStoreProvider` stays (used by both server cache and worker)
- `core/src/sources/` — `FlakePrefetcher` moves to worker (server doesn't clone repos anymore)
- `core/src/nix/evaluator.rs` — `DerivationResolver` trait moves to worker (server doesn't evaluate)
- `core/src/db/` — move all query functions into `repo/` structs

### 2.4 — Split `ServerState` into focused context structs

**Why:** Workers don't need a database. The server doesn't need an eval pool. Each component
should declare exactly what it needs.

```rust
// core/src/types/mod.rs — server context
pub struct ServerContext {
    pub db: DatabaseConnection,
    pub cli: ServerConfig,
    pub log_storage: Arc<dyn LogStorage>,
    pub nix_store: Arc<dyn NixStoreProvider>,      // for cache operations
    pub nar_storage: NarStore,
    pub webhooks: Arc<dyn WebhookClient>,
    pub email: Arc<dyn EmailSender>,
}
```

The `ServerConfig` replaces the current `Cli` for the server binary — it only contains server-relevant
fields (no `eval_workers`, no `binpath_ssh`, etc.).

```rust
// worker/src/config.rs — worker context (no database!)
pub struct WorkerContext {
    pub config: WorkerConfig,
    pub nix_store: Arc<LocalNixStoreProvider>,
    pub eval_pool: Option<Arc<EvalWorkerPool>>,
}
```

### 2.5 — Cache crate stays on server, simplified

The cache crate stays as-is conceptually but uses the new `DerivationRepo` and `CacheRepo` instead
of inline queries. The cache loop runs on the server and operates on the server's local nix store.

Key change: **workers push NARs to the server** (via proto NarPush or S3 PresignedUpload). The
server's cache loop then:
1. Receives NAR from worker (already compressed)
2. Imports into local store
3. Signs (server-side, using the cache signing key)
4. Creates GC root
5. Marks `is_cached = true`

Signing moves to being **optionally** worker-side (if worker has `sign` capability and receives
`SigningKey` credential) or server-side (cache loop signs uncached outputs as today). Worker-side
signing is faster (avoids one round-trip) but server-side is the fallback.

---

## Phase 3: Cleanup & Entity Migration

### 3.1 — Remove `server` / `build_machine` entity entirely

Once all builds go through proto-connected workers, the `build_machine` table is no longer needed.
Workers are tracked in-memory by the `WorkerPool` and identified by their proto `id`.

- New migration: `DROP TABLE build_machine, server_architecture, server_feature;`
- Remove `entity/src/build_machine.rs`, `entity/src/server_architecture.rs`,
  `entity/src/server_feature.rs`
- Remove `web/src/endpoints/servers/` (replaced by worker status in admin API)
- Remove `builder/src/build/queue.rs` `reserve_available_server` (replaced by proto dispatcher)

### 3.2 — Remove `evaluator/` crate from server

After Phase 1 moves evaluation to the worker, the evaluator crate on the server side is just the
thin `EvalScheduler`. Merge it into `scheduler/`:

- Delete `backend/evaluator/` (the crate)
- Its scheduler logic → `scheduler/src/eval_scheduler.rs`
- Its Nix evaluation logic → already in `worker/src/executor/eval.rs`
- `nix-bindings` dependency → only on worker crate

### 3.3 — Remove `builder/` crate from server

Same as evaluator — the build scheduler merges into `scheduler/`:

- Delete `backend/builder/`
- Its scheduler logic → `scheduler/src/build_scheduler.rs`
- Its `SshBuildExecutor` usage → already deleted

### 3.4 — Remove SSH build infrastructure

- Delete `core/src/executer/ssh.rs`
- Delete `core/src/executer/mod.rs` (the `BuildExecutor` trait)
- Remove `binpath_ssh` from CLI
- Remove SSH key management from `core/src/sources/git.rs` (moves to worker)

### 3.5 — Add worker status API endpoints

New endpoints for the admin frontend to monitor connected workers:

```
GET  /api/v1/workers                    — list connected workers
GET  /api/v1/workers/{id}               — worker details + current jobs
POST /api/v1/workers/{id}/drain         — send Draining to a specific worker
POST /api/v1/workers/{id}/disconnect    — force-disconnect a worker
```

These query the in-memory `WorkerPool`, not the database.

---

## Phase 4: Workspace Restructuring

### 4.1 — Final crate layout

```
backend/
├── Cargo.toml              # workspace root
├── src/main.rs             # gradient-server binary
├── core/                   # shared types, repos, storage, CI
│   ├── src/
│   │   ├── lib.rs
│   │   ├── repo/           # database repositories (all DB access)
│   │   ├── types/          # Cli, entity aliases, input validation
│   │   ├── storage/        # LogStorage, NarStore, EmailSender
│   │   ├── ci/             # CI reporter, webhook client
│   │   ├── state_machine/  # build/eval status transitions
│   │   └── nix/            # NixFlakeUrl, Wildcard (utilities only)
│   └── Cargo.toml
├── entity/                 # SeaORM entities (unchanged)
├── migration/              # DB migrations
├── scheduler/              # job creation + dispatch (server-only)
│   ├── src/
│   │   ├── lib.rs
│   │   ├── eval_scheduler.rs
│   │   ├── build_scheduler.rs
│   │   ├── dispatcher.rs
│   │   ├── worker_pool.rs
│   │   └── state_machine.rs
│   └── Cargo.toml
├── proto/                  # wire protocol messages + handler
├── web/                    # HTTP API (Axum)
├── cache/                  # NAR caching + GC (server-only)
├── worker/                 # gradient-worker binary
│   ├── src/
│   │   ├── main.rs
│   │   ├── connection.rs
│   │   ├── handshake.rs
│   │   ├── executor/
│   │   │   ├── mod.rs
│   │   │   ├── fetch.rs
│   │   │   ├── eval.rs
│   │   │   ├── build.rs
│   │   │   ├── compress.rs
│   │   │   └── sign.rs
│   │   ├── job.rs
│   │   ├── scorer.rs
│   │   ├── nar.rs
│   │   └── store.rs
│   └── Cargo.toml
└── test-support/           # shared test fakes + fixtures
```

### 4.2 — Dependency graph

```
entity ← core ← scheduler ← proto ← web ← main (gradient-server)
                    ↑
entity ← core ← worker (gradient-worker)
                    ↑
              evaluator (eval subprocess, linked into worker binary)

test-support depends on: core, entity
```

### 4.3 — Two binaries in Cargo.toml

```toml
[[bin]]
name = "gradient-server"
path = "src/main.rs"

[[bin]]
name = "gradient-worker"
path = "worker/src/main.rs"
```

Or: worker is its own `[package]` with `[[bin]]`.

---

## Phase 5: Testing Strategy

### 5.1 — Principle: all tests stay fully mocked

No test requires a running Nix daemon, a real database, or a real WebSocket connection. Every
external dependency is behind a trait with a fake implementation in `test-support/`.

### 5.2 — Repository tests (core/repo/)

Each repo struct gets unit tests with an in-memory SQLite database (via SeaORM's test support):

```rust
#[tokio::test]
async fn find_next_queued_returns_oldest_first() {
    let db = test_db().await;
    let repo = EvalRepo::new(&db);
    // insert two evaluations with different created_at
    // assert find_next_queued returns the older one
}
```

### 5.3 — Scheduler tests (scheduler/)

```rust
#[tokio::test]
async fn dispatcher_assigns_to_lowest_missing_score() {
    let mut pool = WorkerPool::new();
    pool.register_worker("w1".into(), caps_build());
    pool.register_worker("w2".into(), caps_build());

    let dispatcher = Dispatcher::new(pool);
    dispatcher.receive_scores("w1", vec![score("job-1", 5)], true).await;
    dispatcher.receive_scores("w2", vec![score("job-1", 0)], true).await;

    let assignment = dispatcher.next_assignment().await.unwrap();
    assert_eq!(assignment.worker_id, "w2");
    assert_eq!(assignment.job_id, "job-1");
}
```

### 5.4 — Worker executor tests (worker/)

```rust
#[tokio::test]
async fn execute_flake_job_sends_correct_updates() {
    let nix_store = FakeNixStoreProvider::new();
    let eval_pool = FakeEvalPool::new();
    let executor = JobExecutor::new(Arc::new(nix_store), Some(Arc::new(eval_pool)));
    let (tx, rx) = test_job_updater();

    let job = FlakeJob { tasks: vec![FetchFlake, EvaluateFlake], ... };
    executor.execute_flake_job(job, &mut tx).await.unwrap();

    let updates = rx.collect();
    assert_eq!(updates[0], JobUpdate::Fetching);
    assert_eq!(updates[1], JobUpdate::EvaluatingFlake);
}
```

### 5.5 — Proto round-trip tests (proto/)

Already exist and will continue to validate serialization. Add integration tests that drive a
mock WebSocket through the full handshake + dispatch flow.

### 5.6 — Fakes to add in test-support

- `FakeEvalPool` — returns configured derivation lists without spawning Nix
- `FakeProtoConnection` — records sent messages for assertion
- `FakeWorkerPool` — in-memory worker tracking for scheduler tests
- `FakeNarTransfer` — records NAR push/pull without actual data

---

## Migration Order (Recommended)

Execute phases in this order to keep the codebase compiling and tests passing at every step:

| Step | Phase | Description | Risk |
|------|-------|-------------|------|
| 1 | 0.1 | Introduce repo pattern | Low — additive, no behavior change |
| 2 | 0.2 | Named struct returns | Low — type changes, same data |
| 3 | 0.3 | State machines | Low — additive validation |
| 4 | 0.4 | Rename server → build_machine | Medium — DB migration + many renames |
| 5 | 1.1 | Create worker crate (skeleton) | Low — new code, nothing moved yet |
| 6 | 1.2 | Move eval to worker | High — large code move |
| 7 | 1.3 | Move build to worker | High — large code move |
| 8 | 1.4 | Move signing + compression to worker | Medium |
| 9 | 2.1 | Create scheduler crate | Medium |
| 10 | 2.2 | Implement proto handler | Medium — depends on scheduler |
| 11 | 2.3 | Slim down core | Medium — delete + verify nothing breaks |
| 12 | 2.4 | Split ServerState | Medium — many callsites change |
| 13 | 2.5 | Simplify cache crate | Low |
| 14 | 3.1 | Remove build_machine entity | Medium — DB migration |
| 15 | 3.2 | Remove evaluator crate | Low — already moved |
| 16 | 3.3 | Remove builder crate | Low — already moved |
| 17 | 3.4 | Remove SSH infrastructure | Low — already unused |
| 18 | 3.5 | Worker status API | Low — new endpoints |
| 19 | 4.1-4.3 | Final workspace layout | Low — moves, no logic changes |
| 20 | 5.1-5.6 | Test infrastructure updates | Medium — keep everything green |

**Total: ~20 incremental steps.** Each step should produce a compiling, test-passing codebase.

---

## Key Design Decisions

1. **Workers have no database access.** All persistence flows through proto messages → server →
   database. This means workers are stateless (except local nix store) and can be ephemeral VMs.

2. **Server keeps cache capability.** The server's local nix store is the source of truth for
   cached NARs. Workers push outputs to the server; the server caches and serves them.

3. **No more SSH builds.** `SshBuildExecutor` is deleted entirely. Workers build locally. This
   eliminates the most fragile part of the current codebase (SSH tunneling, socket forwarding,
   remote daemon connections).

4. **Evaluation subprocess pool stays in worker.** The `--eval-worker` subprocess with the Nix C
   API stays — it's the only safe way to run Nix evaluation (thread-unsafe, Boehm GC conflicts).
   But it runs inside the worker binary, not the server.

5. **Testing stays fully mocked.** No test requires Nix, PostgreSQL, or a real WebSocket. Every
   boundary is a trait with a fake. The new repo pattern makes DB tests easier (mock at repo level,
   not at raw query level).

6. **Incremental migration.** Every step compiles and passes tests. The old path (server does
   everything) and new path (server dispatches to workers) can coexist during transition — the
   server can fall back to direct execution while workers are being deployed.
