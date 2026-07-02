# Issue #476: Unify scheduling, self-heal & GC — Implementation Plan

> **For agentic workers:** Executed by the main session (core logic) + Sonnet subagents (mechanical moves). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One legible flow for build-graph scheduling, self-heal, and GC: a single graph reconciler, a single transition entry point with a uniform effects emitter, one readiness/reachability definition, decomposed god-files, a typed dispatch-inputs layer, an explicit scoring weight model, and invariant-preserving GC — as one branch and one PR.

**Architecture:** All build-graph mutation converges on `gradient-db`: bulk sweeps return typed `(derivation, from, to)` transitions consumed by one `emit_transition_effects`; the three hand-curated heal sequences collapse to `reconcile_build_graph(db, storage, scope)`; the readiness predicate and the recursive dependency-closure CTE are defined once and shared by promotion, dispatch, and GC. The scheduler keeps orchestration only (loops, worker pool, scoring); GC deletion clears the gate flags it invalidates in the same transaction.

**Tech Stack:** Rust workspace under `backend/` (sea-orm/Postgres raw-SQL sweeps, tokio), mdBook docs in `docs/src`, NixOS modules in `nix/`.

**Branch:** `refactor/476-unify-scheduling-selfheal-gc` — single PR, commit per task, format `<scope>: <type>: <subject>`.

## Global Constraints

- Behavior-preserving unless a task explicitly says otherwise (the cold-start `ram_free_mb=0` fix and error-propagation fixes ARE behavior changes, deliberately).
- Existing tests stay green: `cargo test --workspace` in `backend/`.
- Persisted/API contracts stay stable: score rule names, `score_breakdown` JSON keys, board API field names, proto wire format. Any new config knob is documented in `docs/src/configuration.md` AND `nix/gradient.nix` module options.
- SQL status literals keep the existing numeric legend; new SQL derives literals from `i32::from(BuildStatus::X)` where practical.
- No — or → characters in commit messages.

## Execution split

- **Main session (Fable):** every task marked `[core]` — new SQL, reconciler, effects emitter, scoring semantics, GC invariants.
- **Sonnet subagents:** tasks marked `[mech]` — file splits, renames, moving code verbatim, test-literal dedup. Each gets exact from/to file paths and the list of items to move; no semantic changes allowed.

---

## Phase 1 — gradient-db: shared graph primitives `[core]`

### Task 1.1: One dependency-closure CTE builder
**Files:** Create `backend/gradient-db/src/graph_sql.rs`; modify `promotion.rs`, `gc.rs`, `lib.rs`.
**Produces:** `pub fn dependency_closure_cte(cte_name: &str, seed_select: &str, direction: ClosureDirection) -> String` (`ClosureDirection::{Dependencies, Dependents}`), plus `pub fn reachable_derivations_cte() -> String` (entry_point ∪ build_job roots, Dependencies direction — replaces `REACHABLE_DERIVATIONS_CTE`).
- [ ] Unit tests pinning the generated SQL shape (mirror existing string-assert tests).
- [ ] Rewrite the recursive CTEs in `cascade_dependency_failed`, `DEPENDENCY_FAILED_RECONCILE_SQL`, `mark_edges_complete_for_eval`, `requeue_failed_closure_for_eval`, `reconcile_cached_anchors_for_eval`, `gc.rs` to use the builder. Existing SQL-shape tests must still pass.

### Task 1.2: One readiness predicate + `find_ready_anchors`
**Files:** Modify `backend/gradient-db/src/promotion.rs`; create `find_ready_anchors` there; modify `gradient-scheduler/src/dispatch.rs` (temporarily, before the split) to call it.
**Produces:** `pub fn deps_ready_predicate(anchor_alias: &str) -> String` (deps terminal-success + closure_complete or substitutable, AND input sources cached) shared by `promote_dependents`, `promote_ready`, and new `pub async fn find_ready_anchors<C>(db: &C) -> Result<Vec<MDerivationBuild>, DbErr>` (the dispatch gate SQL moved out of the scheduler, same ORDER BY).
- [ ] SQL-shape test: promote_ready and find_ready_anchors embed the identical predicate text.
- [ ] Scheduler calls `gradient_db::find_ready_anchors`; inline SQL deleted.

### Task 1.3: edges_complete contract + stale comments
**Files:** `promotion.rs`, `gradient-scheduler/src/build.rs`.
- [ ] Module doc in `promotion.rs`: the flag-discipline table (closure_complete/drv_closure_cached bidirectional; edges_complete monotonic and WHY that is sound — knowledge of a completed edge set never regresses; rows die only by derivation cascade; edges_unresolved covers the dropped-edge case).
- [ ] Delete the contradictory comments describing a demote that "cleared edges_complete" (promotion.rs:434-437, build.rs graph-unstick comment).

## Phase 2 — gradient-db: one transition entry point + effects `[core]`

### Task 2.1: Bulk ops return typed transitions
**Files:** `promotion.rs`, `status/abort.rs`.
**Produces:** `pub struct TransitionChange { pub derivation: DerivationId, pub from: BuildStatus, pub to: BuildStatus }`. `promote_ready`, `promote_dependents`, `cascade_dependency_failed`, `reconcile_dependency_failed`, `reconcile_cached_anchors_for_eval`, and the abort bulk update gain `FROM derivation_build old ... RETURNING db.derivation, old.status AS from_status` self-join so the pre-update status is captured. Callers updated.

### Task 2.2: `emit_transition_effects`
**Files:** Create `backend/gradient-db/src/status/effects.rs`; modify `derivation_build_status.rs`, `status/abort.rs`, `status/mod.rs`.
**Produces:** `pub async fn emit_transition_effects(ctx: &DbContext, changes: &[TransitionChange])` fanning out per change: entry-point dep-count delta (`apply_dep_count_delta` from/to), board `BuildStatusChanged` events per build_job, CI reactor `on_build_status_changed` (entry points only, as today), `CacheChanged` on terminal success. `update_derivation_build_status` is split into persist + the same emitter (single-row path keeps its extra promotion/cascade/attempt/log logic, but event fan-out goes through the emitter). `notify_build_status_for_derivations` reimplemented on top of the emitter (kept as the derivation-keyed convenience wrapper). Abort routes through it.
- [ ] Behavior parity checks: existing scheduler/db tests green.

### Task 2.3: Eval finalization moves to gradient-db
**Files:** Create `backend/gradient-db/src/status/eval_finalize.rs`; modify `gradient-scheduler/src/build.rs`, `gradient-scheduler/src/dispatch.rs`, `lib.rs` re-exports.
**Produces:** `pub async fn check_evaluation_done(ctx: &DbContext, evaluation: EvaluationId) -> Result<(), DbErr-ish>` and `pub async fn finalize_evals_for_derivations(ctx: &DbContext, derivations: &[DerivationId])` (logic moved verbatim from scheduler build.rs:719-794/695-711, incl. eval-error-message check and dep-count resync). Scheduler call sites delegate. Terminal-failure bulk paths can now finalize uniformly.

## Phase 3 — gradient-db: the one reconciler + assertion sweep `[core]`

### Task 3.1: `reconcile_build_graph`
**Files:** Create `backend/gradient-db/src/reconcile.rs`; modify `lib.rs`.
**Produces:**
```rust
pub enum ReconcileScope { Global, Eval(EvaluationId), Unstick(EvaluationId) }
pub struct ReconcileReport {
    pub edges_marked: u64, pub thawed: u64, pub demoted_producers: u64,
    pub cached_reconciled: u64, pub dependency_failed: Vec<TransitionChange>,
    pub promoted: Vec<TransitionChange>,
}
pub async fn reconcile_build_graph(ctx: &DbContext, nar_storage: &NarStore, scope: ReconcileScope) -> ReconcileReport
```
Canonical order (each step logged-and-continue on error, like today):
1. `mark_edges_complete_for_eval` (Eval|Unstick)
2. `requeue_failed_closure_for_eval` (Unstick only)
3. `demote_unbacked_trusted_outputs` (Global|Eval)
4. `reconcile_cached_anchors_for_eval` (Eval|Unstick)
5. `reconcile_drv_closure_cached` (all)
6. `reconcile_closure_complete` (all)
7. `reconcile_dependency_failed` (Global only)
8. `promote_ready` (all)
Effects (`emit_transition_effects` + `finalize_evals_for_derivations` for failed) are applied inside, uniformly.
- [ ] The three call sites (`dispatch.rs` tick block, `eval.rs handle_eval_job_completed`, `build.rs attempt_graph_unstick`) collapse to one call each with the right scope. `handle_eval_job_completed` keeps `seed_entry_point_dep_counts` + eval status advance around the call.

### Task 3.2: Graph-consistency assertion sweep
**Files:** Create `backend/gradient-db/src/consistency.rs`; scheduler loop in Phase 4; config knob.
**Produces:** `pub struct ConsistencyReport { stale_closure_complete: u64, stale_drv_closure_cached: u64, unpromoted_ready: u64, unbacked_trusted: u64, wedged_building_evals: u64 }` + `pub async fn graph_consistency_report<C>(db: &C) -> Result<ConsistencyReport, DbErr>` (read-only COUNT queries reusing the shared gates). Non-zero counts log at `warn`.
- [ ] Knob `GRADIENT_GRAPH_CONSISTENCY_INTERVAL_SECS` (default 300, 0 = off) in `cli/metrics.rs` (+ docs + nix module).

## Phase 4 — scheduler decomposition + inputs layer

### Task 4.1 `[mech]`: split `dispatch.rs` into `dispatch/` module
`dispatch/mod.rs` (start_dispatch_loops, re-exports), `dispatch/background.rs` (worker_liveness_loop, instance_metrics_loop + shutdown select added, worker_sample_loop), `dispatch/eval.rs`, `dispatch/build.rs`, `dispatch/reconcile.rs` (`run_graph_reconciliation` wrapper). Tests move with their code. Named consts `DISPATCH_TICK_SECS`, liveness divisor.

### Task 4.2 `[mech]`: split scheduler `build.rs`
`build/mod.rs`, `build/lifecycle.rs` (output/completed/failed + decide_failure_outcome + backoff), `build/self_heal.rs` (reconcile_missing_inputs, any_reachable), `waiting_state.rs` (reconcile_waiting_state + phase decision + graph-unstick), `buildability.rs` (BuildabilityChecker). Delete `BuildStateHandler` and the thin free-fn wrappers (functions take `&Arc<ServerState>` directly). Eval-finalize functions deleted (moved to db in 2.3).

### Task 4.3 `[mech]`: split `job_handlers.rs` by concern + active-job helpers
`handlers/{queue,assignment,eval_status,build_status,logs,abort}.rs`; add `JobTracker::active_eval_job(job_id) -> Option<PendingEvalJob>` / `active_build_job` and replace the 9 lock+match copies. Fix the stale "ONLY place dispatch_ready_builds runs" comment and lib.rs module doc.

### Task 4.4 `[mech]`: peer_id naming split
Worker-connection ids: parameter/variable `peer_id` → `worker_id` (job_handlers, worker_lifecycle, dispatch, proto handler call sites). Org ownership: `PendingJob::peer_id`/field → `org_id`, `peer_id_for_job` → `org_for_job`, gradient-score `ScoredJob.peer_id` → `org_id`. Wire/API field names unchanged (check `gradient-web/endpoints/orgs/workers.rs` serialized names + proto handshake stay stable).

### Task 4.5 `[core]`: BuildDispatchMaps → batched `DispatchInputs`
- Batch the per-anchor `build_jobs_for_derivation` N+1: new `gradient_db::build_jobs_for_derivations(db, &[DerivationId]) -> HashMap<DerivationId, Vec<MBuildJob>>`.
- Group history queries by distinct pname (one query set, in-memory fan-out); move the `closure_size` backfill out of the loader into an explicit pre-step.
- `DispatchConfig` struct for the scalar config; loader returns `Result` and propagates query errors (no `.unwrap_or_default()` masking).
- `make_pending_job` → `classify_dispatch(...) -> DispatchDecision::{Dispatch, Defer(reason), Skip(error)}` + pure `assemble_job`.
- `dispatch_queued_evals`: bulk `EvalDispatchMaps` (commits, sidecars, overrides, projects in IN-list loads).

### Task 4.6 `[core]`: cold-start metrics + caps unification
- `WorkerShared.cpu_usage_pct`/`ram_free_mb` → `Option<...>` (None until first heartbeat); `WorkerMetricsView` mirrors; `ResourceFitRule`/`ResourceSaturationRule` return 0 with no sample. Board `WorkerInfo` keeps current API shape (None → 0/absent at the view boundary).
- One `pool.worker_caps(worker_id) -> Option<WorkerCaps>` getter replacing the three-getter stitch in `worker_auth_and_caps`; `const BUILTIN_ARCH: &str = "builtin"` shared by dispatch_mode/jobs/dispatch.
- `take_best_of_kind` decomposed: `collect_eligible` (shared with `candidates_for_worker`), `score_candidates`, `select_winner` (named `DISPATCH_FLOOR: f64 = 0.0`), `record_decision`, `assign`; the `.expect` coupling removed (winner carries its detail). `org_work_share` computed only when `policy.uses_org_work_share()`.

### Task 4.7 `[core]`: degraded-vs-absent on scheduler inputs
`on_query_known_derivations` (proto handler): DB errors propagate as an error response instead of empty sets (never re-prunes an `edges_unresolved` anchor on pool exhaustion), probe on `cache_db` pool; `query_for_cache` DbErr no longer swallowed to miss (extends the existing `CacheError` pattern).

## Phase 5 — gradient-score: explicit weight model `[core + mech]`

### Task 5.1: `weights.rs` + stable names + veto
- All magic numbers → named consts in one `weights.rs` module; each rule gains `const NAME: &'static str` (current type names, pinned by test) replacing `type_name` reflection.
- `RuleScore { Value(f64), Veto }`; `RescoreWaitRule` returns Veto (hold) instead of -1000; policy sums values, any Veto ⇒ no dispatch regardless of sum; `ScoreBreakdown` gains `#[serde(default)] vetoes: Vec<String>`; `DISPATCH_FLOOR` documented next to the weights.
- Declarative policy registry: `policy_by_name` builds from a `(rule, enabled)` table; `FairShareRule` stays disabled with its rationale recorded there; `uses_org_work_share()` derived from the table.

### Task 5.2: context cleanup
- Delete `LazyProviders`/`BuildContextLazy` laziness (values are always materialized in production) — pass owned values; `[mech]` for call-site churn.
- `Windowed` fields → `Option<f64>` (measured zero is honored; SQL NULL → None); `w1h_or` = `unwrap_or(fallback)`.
- Thread `now` through `JobContext` (WaitTimeRule stops reading the wall clock).
- Keep `InstanceContext` fields that the board UI displays; delete only fields no rule and no view reads (verify against frontend before deleting).

## Phase 6 — GC: invariant-preserving deletion + registry `[core + mech]`

### Task 6.1 `[core]`: deletion maintains the dispatch-gate invariant
New `gradient_db::clear_gate_flags_for_hashes(db, &[String])` — clears `drv_closure_cached` on anchors whose derivation hash matches and `closure_complete` on producers of matching output hashes. Called inside the same transaction as every `cached_path` deletion: `gc_orphan_derivations` (DB steps wrapped in a txn, storage deletes after commit), `purge_zombie_cached_paths`, `cleanup_stale_cached_nars`, `demote/invalidate` paths.

### Task 6.2 `[core]`: no default-to-delete + set-based sweeps
- `cleanup_stale_cached_nars`: propagate reference-check errors (a row whose check errored is skipped, never reclaimed); replace the hand-rolled per-row cascade with set-based deletes.
- `purge_zombie_cached_paths`: select only (id, hash) columns; anti-join in one pass.
- `record_newly_completed_derivations`: batch the per-derivation N+1.

### Task 6.3 `[core]`: `invalidate_cache_for_path` via demote
Reimplement on `demote_cached_output` (which already resets producers + deletes row/NAR) + `revoke_cache_derivation_closure`, DB steps in one transaction — the producer no longer stays trusted.

### Task 6.4 `[core]`: GC liveness decoupled from wedged evals
`evaluations_to_gc` gains a wedged-eval escape: an "active" eval older than `GRADIENT_GC_WEDGED_EVAL_HOURS` (default 24, 0 = never escape) no longer blocks GC. Same knob consulted by `cleanup_stale_cached_nars`'s active-build exclusion. Docs + nix.

### Task 6.5 `[core]`: honest grace knobs
Split the overloaded `keep_orphan_derivations_hours`: new `GRADIENT_NAR_UPLOAD_GRACE_HOURS` (default 24 — behavior-preserving) governs the orphan-NAR reclaim grace; `keep_orphan_derivations_hours` keeps only the derivation-row grace. Upload lease deferred (see Deferrals). Docs + nix.

### Task 6.6 `[mech]`: sweep registry + test-state builder
- `Sweep { name, interval, run }` registry replacing the monolithic `cache_loop`; per-sweep error isolation + duration logging; intervals configurable: `GRADIENT_CACHE_MAINTENANCE_INTERVAL_SECS` (3600), `GRADIENT_SIGN_SWEEP_INTERVAL_SECS` (60); `deep_gc` reuses the same sweep fns. `eval_cache_sweep` is the template.
- Extract `test_server_state()` builder killing the 6 duplicated `ServerState` literals (cleanup.rs ×5, deep_gc.rs).

## Phase 7 — docs, verification, PR

- [ ] `docs/src/scheduler.md`: rewrite the self-heal section around `reconcile_build_graph`, the transition/effects contract, the flag-discipline table, consistency sweep, GC registry.
- [ ] `docs/src/configuration.md` + `nix/gradient.nix`: all new knobs.
- [ ] `docs/src/tests.md`: new tests documented.
- [ ] `docs/gradient-api.yaml`: verify no API surface changed (goal: none).
- [ ] `cargo test --workspace` green; `cargo clippy` clean on touched crates.
- [ ] PR referencing #476 with the deferral rationale recorded on the issue.

## Deferrals (recorded on #476 at PR time)

1. **Upload lease replacing the 24h grace** — requires wiring lease acquisition through every upload path (presigned S3 issuance, server-side PUT, eval push); a missed path means GC deleting in-flight uploads. Deferred; the knob split (6.5) removes the overloaded-meaning hazard now.
2. **Normalize all rules to bounded [-1,1] signals × weights** — retuning every curve in the same PR as the structural refactor risks silent dispatch-behavior regressions; the weights module + veto sentinel gives one visible tuning surface first.
3. **Derived flags as computed views (gate-on-read)** — superseded in this PR by in-transaction flag clearing on GC deletion (6.1) + the consistency sweep (3.2), per the issue's stated alternative.
4. **`FairShareRule` re-enable** — status made explicit and declarative (5.1); enabling is a scheduling-policy change to make deliberately, not inside a refactor.
