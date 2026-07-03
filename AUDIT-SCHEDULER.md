# AUDIT-SCHEDULER.md - Scheduling, Self-Heal & Garbage Collection

Scope: the scheduling subsystem end to end. `backend/gradient-scheduler` (dispatch loops, build/eval orchestration, information gathering), `backend/gradient-score` (worker-job scoring), `backend/gradient-db` (the build-graph state machine, promotion, reconciliation), and the GC/cache-reclamation surfaces in `backend/gradient-db/src/gc.rs` + `backend/gradient-cache`. Produced by a multi-agent code audit. File:line references are against `main` at audit time.

This is the largest and messiest subsystem, and the primary refactor target.

---

## The core problem: there is no single legible flow

The desired end state is a codebase where the scheduling and self-heal logic reads as a followable flow: one place you can open and see "this is how a build goes from ready to dispatched", "this is how the graph heals itself", "this is how garbage is reclaimed". Today it is the opposite. The same concerns are split across many files and, worse, across two structurally different execution models that must be kept in sync by hand.

Every historical production incident in this subsystem ("stuck Building", "dead zone", "stale-true flag", "InputsUnavailable poison") traces to one root cause:

**A reactive single-row path and a proactive bulk-SQL path both mutate the same build-graph state, and neither is the single source of truth.**

- The reactive path is `update_derivation_build_status` (`gradient-db/src/status/derivation_build_status.rs:24-175`): it validates one transition against the state machine and fans out all the consequences (promotion, closure propagation, dependency-failure cascade, board events, CI reactor, dep-count deltas, log finalization).
- The proactive path is a family of bulk raw-SQL sweeps (`promote_ready`, `cascade_dependency_failed`, `reconcile_*`, `abort_evaluation`) that mutate `status` directly, bypass the state machine, and bypass all the reactive side effects, so every caller must remember to manually re-emit them.

Because healing lives in both models and in neither canonically, the "reconcile then promote" pipeline is copy-pasted at three call sites with three different subsets in three different orders (see DB section, smell 1). GC then adds a seventh independent reimplementation of build-graph reachability (see GC section, smell 1). The result is exactly the "everything everywhere, split up" problem: to reason about one behavior you must read five files and hold two execution models in your head.

### What "good flow" looks like here (the unifying refactor)

These recommendations recur, independently, across all six sub-audits below. They are the concrete shape of the "legible flow" goal and should be treated as one coordinated refactor, not six:

1. **One graph reconciler.** A single `reconcile_build_graph(db, scope)` (scope = `Global` | `Eval(id)` | `Unstick(id)`) that runs the canonical, ordering-correct healing pipeline once. The three current hand-curated sequences (`eval.rs:917`, `dispatch.rs:376`, `build.rs:1034`) each collapse to one call. Every future dead-zone fix has exactly one place to live.
2. **One transition entry point with an attached effects hook.** Make the bulk operations return the set of affected `(derivation, from, to)` transitions and feed them through one `emit_transition_effects(changes)`. Then board events, CI reactor, dep-count deltas, and eval finalization stop being things each caller must remember. It becomes structurally impossible to move an anchor without its consequences firing, which closes the dead-zone class.
3. **One readiness/reachability definition.** The "deps satisfied + closure cached + sources cached" predicate is currently written four times (promotion, dispatch gate, closure gate) and the build-graph walk seven times (Rust BFS x2, single-level refcount, runtime walk, and SQL CTEs in promotion/gc). Consolidate to one query builder / reachability primitive that promotion, the dispatch gate, and GC all consume, so they can never disagree.
4. **One dispatch pipeline, decomposed by phase not by accident.** Split the 1260-line `dispatch.rs` into a small `dispatch/` module (`loops`, `reconcile`, `eval`, `build`, `scoring`) so the flow reads top to bottom.
5. **One information-gathering layer.** Assemble a typed `DispatchInputs` snapshot once per tick instead of stitching six independently-collected streams at assignment time.

The rest of this document is the evidence, per sub-area.

---

# 1. Dispatch algorithm

Scope: `gradient-scheduler/src/{dispatch.rs, dispatch_mode.rs, jobs.rs, worker_pool.rs, worker_lifecycle.rs, worker_state.rs, lib.rs, instance.rs, job_handlers.rs}`. Note: this "pool" is the registry of connected remote workers, not the eval-worker subprocess pool. There is no classic "checkout"; workers register, are filtered by capacity/caps, and pull work via `RequestJob`.

## Current flow

Three decoupled phases connected by an in-memory `JobTracker` and a `watch`/`Notify` pair. DB rows are the source of truth for readiness; the tracker is a transient cache of offerable work.

```
PHASE A - ENQUEUE (DB rows -> in-memory pending)         [dispatch.rs]
build_dispatch_loop (dispatch.rs:354)  every 5s  OR  dispatch_kick (Notify)
  |
  +- if TIMER tick (not a reactive kick):                (dispatch.rs:370-461)
  |    job_tracker.bump_rescore_counts()                 (jobs.rs:906)
  |    reconcile_drv_closure_cached()       -- 5 unrelated DB
  |    reconcile_closure_complete()            "dead-zone" sweeps
  |    demote_unbacked_trusted_outputs()       crammed into the
  |    reconcile_dependency_failed()           dispatch tick
  |    promote_ready()                         (Created -> Queued backstop)
  |
  +- requeue_transient_failures (dispatch.rs:490)  FailedTransient past backoff -> Queued
  +- dispatch_ready_builds (dispatch.rs:1009)  ALSO called on-demand (Phase C)
  |    1. raw SQL: Queued AND edges_complete AND reachable-by-build_job AND
  |       (substitutable OR (drv_closure_cached AND all deps ready AND input_sources cached))
  |       ORDER BY dep_count DESC, updated_at ASC          (dispatch.rs:1027-1078)
  |    2. drop anchors already in tracker (contains_job)   (dispatch.rs:1091)
  |    3. stamp ready_at where NULL                        (dispatch.rs:1103)
  |    4. BuildDispatchMaps::load(...)  bulk IN-list loads (dispatch.rs:555)
  |    5. per anchor: maps.make_pending_job(anchor)        (dispatch.rs:889)
  |         decide_dispatch_mode() + backoff gate (may return None)
  |         enqueue_build_job(job_id, pending)             (job_handlers.rs:48)
  +- reconcile_waiting_state()                           (worker_lifecycle.rs:290)

eval_dispatch_loop (dispatch.rs:153)  every 5s -> dispatch_queued_evals (dispatch.rs:171)

PHASE B - OFFER (pending -> worker), driven by the worker session loop
job_notify bump -> get_new_job_candidates(worker_id) (job_handlers.rs:99)
  candidates_for_worker(authorized, caps) (jobs.rs:428) minus pool.sent_candidates_for
  server pushes JobOffer(delta) -> worker probes local store -> CandidateScore[]
  record_scores(worker_id, scores) (job_handlers.rs:184 -> jobs.rs:446)

PHASE C - ASSIGN (worker pulls a free slot)
worker sends RequestJob{kind} -> request_job (job_handlers.rs:133)
  pool.has_capacity + worker_auth_and_caps + try_assign (job_handlers.rs:731)
    job_tracker.take_best_of_kind(...) (jobs.rs:509)
      re-filter eligible pending -> score each (org_work_share + policy.score_detailed)
      sort desc, winner = best iff total >= 0.0 (jobs.rs:633)  [magic gate]
      assign_pending (pending -> active), pool.assign_job, spawn persist_dispatched_job
```

Structural fact: `dispatch_ready_builds` has two callers (the 5s loop and the on-demand path in `request_job`), despite the comment at `job_handlers.rs:156-158` asserting it is "the ONLY place `dispatch_ready_builds` runs for build jobs."

## Data structures & state

All on `Scheduler` (`lib.rs:61-94`): two coarse `RwLock`s (`worker_pool`, `job_tracker`) plus `ArcSwap`/`watch`/`Notify`. Lock acquisitions are sequential, never nested, so no deadlock ordering hazard, but that same non-atomicity is the main race surface.

- `pending`/`active`/`scores`/`decisions` in `JobTracker` under one `RwLock` (`jobs.rs:396-401`); `scores` is the offer cache that goes stale when a worker's store changes, healed only by a re-offer bump (`dispatch.rs:482`).
- `sent_candidates` per worker under the pool `RwLock` (`worker_state.rs:82`): a third per-job bookkeeping store in a different lock from `pending`/`scores`, kept consistent by hand.
- `job_notify` (`watch`, `lib.rs:70`), `dispatch_kick` (`Notify`, `lib.rs:75`), `instance`/`eval_history` (`ArcSwap`, `lib.rs:85-89`), `draining` (`AtomicBool`).

Race surfaces: capacity TOCTOU (`request_job` checks `has_capacity` under a released read lock then assigns under separate locks, `job_handlers.rs:135-141`); split-brain offer state (`sent_candidates` in pool lock vs `pending` in tracker lock); swallowed-DB-error staleness (`BuildDispatchMaps::load` turns nearly every failed query into an empty map via `.unwrap_or_default()`, `dispatch.rs:568,650,685,720,743`, so "query failed" is indistinguishable from "no inputs", the known false-`InputsUnavailable` class).

## Messiness & code smells (ranked)

1. **`peer_id` is overloaded to mean both "worker connection id" and "organization id".** `take_best_of_kind(peer_id, ...)` uses it as the worker key (`jobs.rs:520`), and `record_scores`/`register_worker`/`worker_disconnected` all pass the worker id, yet `PendingJob::peer_id()` (`jobs.rs:159`) and `authorized_peers: HashSet<OrganizationId>` mean the org. The same identifier name denotes two entities across the hottest functions. Single biggest comprehension hazard in the module.
2. **`dispatch.rs` is a ~1260-line god file mixing five responsibilities**: background-loop orchestration (6 spawned loops incl. `worker_liveness_loop`, `instance_metrics_loop`, `worker_sample_loop`, which are not dispatch, `:66-149`), eval dispatch (`:171-350`), build dispatch (`:354-1150`), the 300-line `BuildDispatchMaps` loader (`:515-999`), embedded raw SQL (`:1027-1078`), and tests.
3. **`BuildDispatchMaps::load` is a 310-line procedure mixing bulk reads, per-row DB writes, N+1 awaits, config, and silent error-swallowing** (`dispatch.rs:555-866`): N+1 `build_jobs_for_derivation` per anchor (`:577-582`) and `history::predict` per derivation (`:829-836`); writes `closure_size` back mid-load (`:819-828`); 25 fields mixing maps with scalar config; `.unwrap_or_default()` hides pool exhaustion as "absent".
4. **`JobTracker::take_best_of_kind` is a ~225-line method** (`jobs.rs:509-734`) doing eligibility, scoring, winner selection, decision recording, record building, and assignment, with a `.expect("a winner implies its scored detail exists")` (`:693`) that only holds because two separate `total >= 0.0` filters (`:633` and `:670`) are kept in sync by hand. The magic dispatch gate `total >= 0.0` (`:633`) is an undocumented policy decision embedded in tracker mechanics.
5. **Duplicated candidate-eligibility predicate** (`jobs.rs:428-441` vs `:609-620`): `candidates_for_worker` and `take_best_of_kind` independently reimplement "authorized peer AND `job_eligible_for_caps`".
6. **The 5-reconcile dead-zone sweep is inlined into the dispatch tick** (`dispatch.rs:370-461`), coupling "keep graph invariants sound" to "offer work" and making the loop untestable without a full DB.
7. **Embedded raw SQL in the scheduler** (`dispatch.rs:1027-1078`): the most safety-critical predicate in the system (whether a build's inputs are really present) is a 50-line inline correlated-subquery string, while every other DB concern goes through named `gradient_db::*` functions.
8. **`make_pending_job` conflates "how to build" with "whether to dispatch", using `None` as a side-channel** (`dispatch.rs:889-998`): returns `None` for both hard errors and a soft "back off, re-probe later" (`:921-931`), so the caller can't tell a bug from a deliberate defer.
9. **Three overlapping worker-capability structs** (`WorkerShared` `worker_state.rs:64`, `WorkerCaps` `jobs.rs:105`, `WorkerMetricsView` `worker_pool.rs:230`), stitched by hand in `worker_auth_and_caps` (`job_handlers.rs:701-727`) from three separate pool getters.
10. **The "builtin" runs-anywhere rule and its magic string are duplicated** across `dispatch_mode.rs:21`, `jobs.rs:123-124`, `dispatch.rs:958`.
11. **`dispatch_queued_evals` is a ~120-line function with per-eval N+1 loads** (`dispatch.rs:171-293`); the build path was bulk-loaded but the eval path never was.
12. **Stale/contradictory doc comments** (`lib.rs:11-13` claims impl is only in two modules; `job_handlers.rs:156-158` claims a unique call site that is not unique).
13. **Redundant test coverage** (`dispatch.rs:1186-1194` re-tests `decide_dispatch_mode` already covered in `dispatch_mode.rs:41-78`).
14. **Repeated lock churn in the metrics loops** (`dispatch.rs:104-106`, `139-143`).
15. **Magic numbers scattered without named constants**: `Duration::from_secs(5)` in both loops (`:154`, `:355`), `(timeout_secs / 3).max(5)` (`:75`), `total >= 0.0` (`jobs.rs:633`), `max_concurrent_builds` default 1 (`worker_state.rs:133`).

## Refactoring recommendations

1. **Fix the `peer_id` naming split** (highest clarity-per-line): rename worker-connection id to `worker_id` everywhere, org id to `owner_org`/`org_id`, introduce newtypes so the compiler enforces the distinction. Pure rename, no behavior change.
2. **Split `dispatch.rs` into a `dispatch/` module**: `loops.rs` (the spawns), `reconcile.rs` (extract the dead-zone sweep into one `run_graph_reconciliation`, see the cross-cutting reconciler rec above), `eval.rs` (+ an `EvalDispatchMaps` bulk loader to kill the eval N+1), `build.rs`. Move `worker_liveness_loop`/`worker_sample_loop`/`instance_metrics_loop` out of dispatch into `background.rs`/`metrics.rs`.
3. **Introduce a `BuildJobAssembler` and push its SQL into `gradient_db`**: move the ready-anchor query into `gradient_db::find_ready_anchors`; split `BuildDispatchMaps::load` into small single-concern loaders (batch the per-anchor `build_jobs_for_derivation` into one IN-list query); separate config scalars into `DispatchConfig`; move the `closure_size` back-persist out of the loader; propagate query errors instead of `.unwrap_or_default()`.
4. **Decompose `take_best_of_kind` into a pure scoring pipeline** (`collect_eligible` shared with `candidates_for_worker`, `score_candidates`, `select_winner` behind a named `DISPATCH_FLOOR` const, `record_decision`, `assign`), leaving `JobTracker` as pure state.
5. **Unify worker capability representation** into one `pool.worker_caps(worker_id)`; introduce an `Architecture` abstraction (or `const BUILTIN`) so the runs-anywhere rule lives in one place.
6. **Split `job_handlers.rs` by concern** (`queue`, `assignment`, `eval_status`, `build_status`, `logs`, `abort`, `dispatched_job`).
7. **Make readiness-vs-how-to-build an explicit two-step**: `classify_dispatch -> Dispatch | Defer(reason) | Skip(error)` then a pure `assemble_job`, killing the `None` side-channel.
8. **Extract named constants / config** (`DISPATCH_TICK`, `DISPATCH_FLOOR`, liveness divisor), sourcing tick intervals from config since rescore semantics depend on them.

---

# 2. Build & eval orchestration

Scope: `gradient-scheduler/src/{build.rs (1992), eval.rs (1249), job_handlers.rs (910), trigger_dispatch.rs, eval_metrics.rs, history.rs, views.rs, log_substitution.rs}`. The persisted transition rules live in `gradient-db/src/state_machine/{build,eval}.rs` (pure validators) applied in `gradient-db/src/status/derivation_build_status.rs`.

## Current flow

Evaluation lifecycle: `trigger_dispatch_loop` (`trigger_dispatch.rs:106`, 5s) creates a Queued eval; `eval_dispatch_loop` (`dispatch.rs:171`) builds a `FlakeJob` and enqueues it; the worker streams `DiscoveredDerivations`; `handle_eval_result` (`job_handlers.rs:352` -> `eval.rs:804`) inserts derivations, persists input sources, and `resolve_anchors` upserts anchors + build_jobs; `handle_eval_job_completed` (`eval.rs:902`) runs 8 sequential DB self-heal/promotion calls in a hardcoded order, sets the eval Building, and `check_evaluation_done` (`build.rs:719`) makes the graph-derived terminal decision. In parallel, `reconcile_waiting_state` (`build.rs:810`) parks/unparks evals on every dispatch tick.

Build lifecycle: `resolve_anchors` (`eval.rs:302`) creates anchors Created/Substituted; `promote_ready` (`dispatch.rs:447`) promotes Created to Queued once deps terminal-success; `dispatch_ready_builds` (`dispatch.rs:1009`) applies the readiness gate; `request_job`/`try_assign` opens a `build_attempt`; on completion `handle_build_job_completed` (`build.rs:267`) sets Completed/Substituted and promotes deps; on failure `handle_build_job_failed` (`build.rs:375`) classifies via `decide_failure_outcome` (`build.rs:47`) into Retry/Requeue/Permanent/Timeout/InputsUnavailable; transient failures re-queue after backoff (`requeue_transient_failures`, `dispatch.rs:490`).

Failure/retry reporting is split across three modules and two directions (worker RPC, reactive cascade in `update_derivation_build_status`, proactive sweeps in `build_dispatch_loop`), see the root-cause section above.

## Messiness & code smells (ranked)

1. **`build.rs` is a 1992-line grab-bag of five unrelated concerns**: (i) build lifecycle handlers; (ii) the missing-input self-heal engine (`reconcile_missing_inputs`, `any_reachable`); (iii) evaluation finalization (`check_evaluation_done`, `check_referencing_evals_done`, `finalize_evals_for_derivations`, eval logic living in `build.rs`); (iv) the waiting-state reconciler; (v) the `BuildabilityChecker`. This is exactly the file where the recurring dead-zone bugs live.
2. **`reconcile_missing_inputs` is a 155-line god function** (`build.rs:521-675`) interleaving four distinct recovery strategies (purge output / demote referrers / rewalk deps / requeue failed) in one loop with five mutable accumulators. Highest-risk function in the layer, essentially untestable as written.
3. **`resolve_anchors` is a 209-line function doing six jobs** (`eval.rs:255-463`), including two near-duplicate `update_many` blocks that flip `substitutable` true (`:410-427`) and false (`:434-455`).
4. **`reconcile_waiting_state` is a 148-line nested-match state engine** (`build.rs:810-957`) that decides eval-machine transitions by ad-hoc matching, separate from the `EvalStateMachine` validator, with reason ownership encoded as a hand-maintained `matches!` list (`:874`).
5. **`handle_build_job_failed` mixes classification, side effects, and terminal transition** (`build.rs:375-507`), ending in a "belt-and-braces" `cascade_dependency_failed` (`:505`) the comment admits is redundant, a sign the author didn't trust the reactive path.
6. **`BuildDispatchMaps::load` per-anchor DB call in a bulk loader** (`dispatch.rs:578`).
7. **~9 copies of the "resolve active job" lock+match+early-return boilerplate** (`job_handlers.rs:206,239,267,359,416,534,632,667`, `eval_metrics.rs:23`).
8. **`/nix/store/` prefix stripping done twice per eval result** (`job_handlers.rs:379-384` then again `eval.rs:815-820`), a latent divergence bug.
9. **Duplicated worker-cap satisfaction logic across three places** (`build.rs:1456`, `build.rs:1513`, `dispatch.rs:914`).
10. **Thin free-function wrappers duplicate the handler surface** (`build.rs:1270-1332`): six free fns each just `BuildStateHandler::new(state).method(...)`, paying for both a struct and a wrapper.
11. **Fire-and-forget `tokio::spawn` side effects with no tracking** (`build.rs:293`, `eval.rs:791`, `derivation_build_status.rs:80,135,151,170`), a known source of races (the #399 window is commented at `build.rs:247-254`).
12. **Raw multi-level SQL string inline in `dispatch_ready_builds`** (`dispatch.rs:1027-1078`), duplicating in SQL the same "deps ready" logic `promote_ready` expresses.

## State-transition complexity

Two machines, three enforcement layers, and the reactive/proactive split that is the root of the dead-zone class:
- The state-machine validators (`state_machine/build.rs:44`, `eval.rs:45`) are pure and correct but only consulted by the single-row path.
- Side effects are welded to the single-row path (`derivation_build_status.rs:100-172`).
- Bulk graph mutations bypass both and must manually re-emit effects (`dispatch.rs:423/429`, comment at `build.rs:690-694` describing the "eval hangs Building" dead zone directly).

Specific fragilities: eval finalization decided reactively (`check_evaluation_done`) AND proactively (`finalize_evals_for_derivations`); the reconciler set copy-pasted with divergent ordering at three sites (`eval.rs:917`, `dispatch.rs:376`, `build.rs:1034`); `closure_complete`/`drv_closure_cached` needing perpetual bidirectional reconciliation; waiting-reason ownership as a hand-maintained partition (`build.rs:874`); at least six independent routes that reset an anchor backward with unaudited interactions.

## Refactoring recommendations

1. **Split `build.rs` (1992 -> ~5 modules)**: `build/lifecycle.rs`, `build/self_heal.rs`, `eval_finalize.rs` (eval logic does not belong in build.rs), `waiting_state.rs`, `buildability.rs`. Delete the `BuildStateHandler` wrapper and thin free-fn wrappers.
2. **One `GraphReconciler`** replacing the three copy-pasted sequences (the cross-cutting rec).
3. **Decouple transitions from side effects behind a single entry point** with a post-hook that bulk paths route through (the cross-cutting rec) - this closes the dead-zone class structurally.
4. **Model `WaitingReason` ownership as data** (`owner: ReasonOwner`), removing the hand-synced guard lists.
5. **Extract the readiness gate into one testable source of truth** shared by `dispatch_ready_builds` and `promote_ready`.
6. **Collapse failure/retry/requeue into one `plan_recovery(anchor, cause) -> RecoveryAction`** + one applier; extend `decide_failure_outcome` to be the decision point.
7. **De-duplicate the mechanicals**: `active_eval_job`/`active_build_job` helpers; strip prefixes once; fold the two `substitutable` flips into `set_substitutable`; extract the `arch_ok && feats_ok` predicate.

---

# 3. Scoring engine

Scope: `gradient-score` (`policy.rs`, `rule.rs`, `context.rs`, `breakdown.rs`, `rules/*`) + its scheduler integration. A pure additive rule-sum with no shared normalization: every rule returns an `f64` and the policy sums them.

## Current flow

`ScoreRule` (`rule.rs:33`) is the unit: `score(job, worker, instance) -> f64`. `name()` is derived by reflection from `std::any::type_name` (`rule.rs:40-43`), so the Rust type name is the persisted map key. `RulePolicy` (`policy.rs:42`) holds a `Vec<Box<dyn ScoreRule>>` and sums via `score_detailed` (`policy.rs:69`), recording per-rule contributions into a `ScoreBreakdown` serialized to `dispatched_job.score_breakdown`. Rule sets are hardcoded functions (`simple_rules`, `resource_aware_rules`) selected by a `policy_by_name` match (`policy.rs:124`). Context has three layers: `JobContext` (per-candidate), `ScoredJob`/`BuildContextLazy` (job attributes, with `closure_size`/`history` behind `OnceCell`/`Cell`), and `InstanceContext` (18 windowed cluster fields). The scheduler drives it in `take_best_of_kind` (`jobs.rs:509`): builds `WorkerContext`, computes `org_work_share` over all active builds per-request (`jobs.rs:538-554`), scores each eligible job, and picks the first with `total >= 0.0` (the implicit dispatch floor, `jobs.rs:633`).

## Rule inventory (13 rules)

`MissingPathsRule` (cap 200), `MissingNarSizeRule` (cap 500), `DependencyCountRule` (cap 50), `WaitTimeRule` (anti-starvation, cap 4000, reads wall clock in `score()`), `BuiltinDeprioritizeRule`, `ReserveFetchWorkersRule` (-300), `RescoreWaitRule` (-1000 hold until measured), `ResourceFitRule` (RAM overshoot to -1600), `ResourceSaturationRule` (-1000 stackable to -2000), `PreferLocalBuildRule` (+150), `NetworkAffinityRule` (+80, w24h), `DiskAffinityRule` (+60, w24h), `FairShareRule` (dead: commented out of the policy at `policy.rs:107-108` but compiled, exported, and fully tested).

## Messiness & code smells (ranked)

1. **All weighting is implicit in scattered magic numbers with no shared scale**: `4000, 1000, 1000, 500, 500, 400, 300, 200, 150, 100, 80, 60, 60, 50, 50, 20` across six files. Tuning "should wait beat cache-warmth?" means reading all six and mentally simulating cap interactions. Cross-rule invariants are encoded as brittle test asserts ("WaitTime must out-budget MissingPaths" `policy.rs:186`, "RAM penalty under WaitTime cap" `resource.rs:183`) that rot silently if a constant changes.
2. **The dispatch floor `total >= 0.0` (`jobs.rs:633`) is load-bearing but invisible**: "don't dispatch yet" is expressed by making the sum go negative, so whether a job crosses zero depends on unrelated magic constants.
3. **The `LazyProviders`/`OnceCell`/`Cell` machinery is dead weight in production** (`context.rs:115-151`): in the real path both `closure_size` and `history` are already materialized on `PendingBuildJob`; the laziness is never exercised.
4. **~12 copies of the same guard boilerplate**, no shared combinator (`let Some(b) = job.job.build() else { return 0.0 }` in 7 rules; `let Some(m) = worker.metrics else { return 0.0 }` in 4).
5. **Rule identity is a Rust type name via reflection** (`rule.rs:40-43`): renaming a struct silently changes the DB/API contract (`score_breakdown` keys, `rule_catalog`).
6. **`FairShareRule` is shipped-but-unwired dead code** (`policy.rs:107-108`), and it owns the only consumer of `org_work_share`, which the scheduler still computes on every request.
7. **`org_work_share` is assembled per-request inside `take_best_of_kind`** (`jobs.rs:538-554`), O(workers x active_builds) under contention, feeding only a disabled rule.
8. **Triple bookkeeping for instance metrics** (SQL alias / row struct / context field), and several `InstanceContext` fields are read by no rule (`peak_ram_mb`, `avg_cpu_pct`, `closure_size`, `completed`, `active_builds`, `pending_builds`).
9. **`Windowed::w1h_or/w24h_or` treat 0.0 as "absent"** (`context.rs:17-23`), so a measured zero swaps in a magic fallback; rules disagree on which window to trust with no documented rationale.
10. **Redundant / test-only surfaces**: `BuildContext`/`aggregate`/`EvalContext` (`context.rs:75-124`) used only in tests; `ScoringPolicy::score`/`RulePolicy::score` used only in tests.
11. **`WaitTimeRule` reads the wall clock inside `score()`** (`builtin.rs:173`), making the rule non-deterministic.

## Refactoring recommendations

1. **Introduce an explicit weight/normalization model**: split each rule into a bounded signal in `[-1.0, 1.0]` and a weight; collect weights into one `ScoringWeights` struct (or TOML) so priorities are visible and tunable in one place. Make "hold, don't dispatch" an explicit `RuleOutcome::Veto`/`min_dispatch_score` sentinel instead of an emergent property of summed magic numbers.
2. **Make rule names explicit and stable** (`const NAME`), dropping the `type_name` reflection.
3. **Collapse the guard boilerplate with sub-traits** (`BuildScoreRule`, `MetricsRule` with blanket `impl ScoreRule` doing the guard once).
4. **Separate context assembly from scoring and hoist it out of the hot loop**: move `org_work_share` into the periodic metrics loop; delete the unused `LazyProviders` (pass owned values) or make it genuinely lazy.
5. **Make the policy/rule set declarative** (a `policy_name -> Vec<(rule, weight)>` registry), resolving `FairShareRule`'s status explicitly.
6. **Prune and consolidate `InstanceContext`**: delete fields no rule reads, reduce the SQL/row/context triple to one source, replace the `0.0 == absent` heuristic with `Option`-typed windows.
7. **Thread current time through the context** instead of `WaitTimeRule` calling `now()` internally.

---

# 4. Information gathering (the scheduler's inputs)

The dispatch decision (`take_best_of_kind` -> `policy.score_detailed`) is fed by six independent information streams collected on five cadences by six background loops (`start_dispatch_loops`, `dispatch.rs:41-55`). No single place assembles them; each lands in its own store (an `ArcSwap`, the pool, the tracker, or is recomputed inline per request), and scoring stitches them together at assignment time. This is the "everything everywhere" problem applied to the inputs.

## Sources

| Source | What | Collected by | Cadence | Stored | Consumed by |
|---|---|---|---|---|---|
| Worker static caps | archs, features, max_concurrent_builds, cpu/ram | `on_worker_capabilities` -> `update_capabilities` (`worker_pool.rs:189`) | once at handshake | `WorkerShared` | `can_build`, capacity, `ResourceFitRule` |
| Worker live metrics | cpu%, ram_free, disk/network mbps | `on_worker_metrics` -> `update_metrics` (`worker_pool.rs:211`) | 10s heartbeat (hardcoded `worker/dispatch.rs:67`) | `WorkerShared` | resource/affinity rules |
| Per-candidate cache score | missing_count, missing_nar_size | worker `JobScorer::score_candidates` (`worker/proto/scorer.rs:39-70`) | event-driven, batched | `JobTracker.scores` | missing-paths rules, rescore gate |
| Instance window metrics | 13 windowed aggregates + live counts | `compute_instance_context` (`instance.rs:77`, two raw-SQL) | 30s (`GRADIENT_INSTANCE_METRICS_INTERVAL`) | `ArcSwap<InstanceContext>` | all windowed rules |
| Build history | p95 RAM, avg cpu/disk/time per derivation | `history::predict` (`history.rs:35`) | per dispatch pass, per new anchor | `BuildDispatchMaps.histories` | `ResourceFitRule`, `DiskAffinityRule` |
| Closure size | transitive output NAR bytes | `transitive_closure_sizes` (`closure.rs:116`) | lazy, when NULL | `derivation.closure_size` | near-unused (only the predict bucket key) |
| Graph facts | derivations, edges, cache_info, features | `BuildDispatchMaps::load` (`dispatch.rs:553`) | per pass, new anchors | transient maps | `make_pending_job`, `can_build` |

## Freshness / staleness (ordered by how wrong a decision it produces)

1. **Cold-start `ram_free_mb = 0` reads as fully saturated (correctness bug).** Live metrics default to zero (`worker_state.rs:137-140`) until the first 10s heartbeat, and `metrics_for` returns `Some` for any connected worker. `ResourceSaturationRule` computes `0/total <= 0.10` and applies -1000 (`resource.rs:104-108`), so a freshly-connected idle worker is scored as RAM-saturated for its first ~10s. Root cause: representing "no sample yet" as 0 for RAM/CPU while disk/network correctly use `Option`.
2. **30s-stale cluster counts gate live fairness decisions**: `FairShareRule` gates on `idle_workers > 0` (`fair_share.rs:35`) from the 30s snapshot even though `try_assign` holds the live pool lock.
3. **`org_work_share` recomputed per request against a stale weight** (`jobs.rs:538-554`): live share, 30s-stale weight, 30s-stale idle-gate, three freshness domains in one score.
4. **Pending-job graph facts frozen at first dispatch**: `cache_info`, `closure_size`, `dependency_count`, `history` are a point-in-time snapshot; later passes only bump `rescore_count`.
5. **Disk/network throughput never decay** (`throughput.rs:52`): a worker that transferred once at 500 Mbps reports 500 forever; `disk_speed_mbps` is never sampled by default (requires `build_metrics=true`), so `DiskAffinityRule` is dead by default.
6. Silently-dead worker keeps stale metrics ~40s; `derivation.closure_size` written once and never invalidated; startup reads the all-zero default `InstanceContext` for the first tick.

**Error-swallowing that corrupts inputs.** The main CacheQuery-DbErr-reads-as-absent bug is fixed on the build-prefetch path (`CacheError` message, isolated `cache_db` pool). Still live: `query_for_cache` swallows `DbErr` into a miss (`cache.rs:690-695`); `on_query_known_derivations` swallows errors as empty (`dispatch.rs:1176,1186,1198`) and the `edges_unresolved` load `unwrap_or_default`s (`:1191-1199`), which can re-prune an `edges_unresolved` anchor and reopen the dead zone `4b05dfe5` closed; scoring `missing_nar_size` silently drops to 0 when `cache_info` is `None`.

## Messiness & code smells (ranked)

1. CacheQuery prefetch storm: per-build closure walk, one full `CacheQuery` RPC per wave up to `MAX_ITERATIONS = 1024` (`nar_import.rs:628-661`) across every in-flight build. Dominant scheduler-input load; mitigated (isolated pool, budget, semaphore), not eliminated.
2. N+1 on the live dispatch path: `build_jobs_for_derivation` per anchor (`dispatch.rs:577-582`) while every sibling load is batched. Highest-value quick win.
3. N+1 history queries per derivation, serial, undeduped (`dispatch.rs:832-836`); filters by `pname` so anchors sharing a pname fire redundant identical queries.
4. Serial DB round-trips inside `BuildDispatchMaps::load` and the CacheQuery handler that could be `try_join!`ed/batched.
5. Quadruple-declared worker-metric fields (proto / `WorkerShared` / `WorkerMetricsView` / `WorkerInfo` + `WorkerContextView`), threaded positionally with four `too_many_arguments` allows.
6. Triple/quadruple-declared instance-metric fields (SQL alias / row field / context assignment / test column-map); a rename must touch all four or silently deserialize to 0.
7. Collected-but-unused metrics: `cpu_count`, six `InstanceContext` fields, `.w5m` window (never read), most of `.w24h`.
8. Mixed collection cadences with no coordinating layer; the 10s worker heartbeat is hardcoded while the server timeout is configurable (setting timeout below ~10s evicts live workers).
9. Expensive readiness SQL fired at kick frequency (three nested `NOT EXISTS` + correlated `count(*)` in `ORDER BY`).
10. Eval-path N+1 writes (`add_system_features`/`add_features` O(derivations x features), `compute_upstream_substitutable` per-output update).
11. `instance_metrics_loop` has no shutdown-token select (`dispatch.rs:102-124`).

## Refactoring recommendations

1. **Introduce a `DispatchInputs` snapshot assembled once per pass**: batched IN-list loads for the new-anchor batch run concurrently with `try_join!`; history as one grouped query with in-memory fan-out; single-statement closure backfill.
2. **Unify each metric to a single declaration** (one struct passed by value; derive the SQL projection and score-facing view from it), killing the quadruple/triple declarations.
3. **Model "no sample yet" and "degraded" as types, not zero**: make live RAM/CPU `Option` (fixes the cold-start correctness bug); extend the `CacheError` degraded-vs-absent pattern to `query_for_cache` and `on_query_known_derivations`, and route the known-derivations probe onto the isolated `cache_db`.
4. **Move per-request work into the periodic collector; read live counts live** (fairness reads `idle_workers` under the lock it already holds).
5. **Prune the collectors to what scoring consumes.**
6. **Refresh or explicitly freeze the pending-job snapshot** for long-waiting jobs; give the throughput EWMA a time decay; derive the heartbeat interval from the negotiated timeout; add a shutdown-token select to `instance_metrics_loop`.

Highest-leverage first: fix cold-start `ram_free_mb` (correctness), batch the `build_job` N+1, read cluster counts live for fairness. The unified `DispatchInputs` collector is the larger structural cleanup.

---

# 5. DB state-machine, promotion & reconciliation (the self-heal core)

Scope: `gradient-db/src/{promotion.rs, cache_storage.rs, status/derivation_build_status.rs, state_machine/*, dep_closure.rs, closure.rs, recovery.rs, gc.rs, build_attempt.rs}`. This is where build-graph correctness lives and where the dead-zone bug class originates.

## State model

`derivation_build` carries `status` (state-machine-guarded OR bulk raw SQL) plus derived flags with inconsistent maintenance discipline:

| Flag | Discipline | Stale-true possible? | Heal |
|---|---|---|---|
| `closure_complete` | bidirectional CLEAR->SET fixpoint (`promotion.rs:206`) | guarded | `reconcile_closure_complete` |
| `drv_closure_cached` | bidirectional CLEAR->SET fixpoint (`promotion.rs:287`) | guarded | `reconcile_drv_closure_cached` |
| `edges_complete` | monotonic set-only (`promotion.rs:441`, "never clears it") | yes, unguarded | none - no CLEAR pass |
| `cached_path.closure_complete` | manual clear-only, no SET fixpoint (`cache_storage.rs:436`) | asymmetric | `clear_closure_complete_for_referrers` |

`BuildStatus`: `Created=0 Queued=1 Building=2 Completed=3 FailedPermanent=4 Aborted=5 DependencyFailed=6 Substituted=7 FailedTransient=8 FailedTimeout=9`. Legality in `state_machine/build.rs:43-99` (a pure validator that does not persist).

## Reconciliation & dead-zone patterns (the crux)

Two archetypes account for nearly every historical incident:
- **Archetype A - monotonic set-once flag goes stale-true.** A gating flag is set once and never cleared; ground truth then regresses (GC deletes a NAR, an output is evicted, an edge is recorded late); the gate dispatches a build whose inputs are not actually present, giving terminal `InputsUnavailable` that poisons the dependent closure. Post-mortems are in the code: `promotion.rs:184-205` (closure_complete), `promotion.rs:273-286` (drv_closure_cached).
- **Archetype B - reactive-only transition never fires.** A heal exists only as a reaction to a fresh transition event; if the triggering event already happened (anchor thawed after its dep failed, completed under older code, or across a restart), nothing re-fires and the node sits wedged. The textbook case is `demote_unbacked_trusted_outputs` (`cache_storage.rs:368-401`): the reactive heal is structurally unreachable because the dead-zone anchor never dispatches, so only a proactive sweep keyed on ground truth rescues it.

Every reactive heal has (or needed) a proactive twin added later: `promote_dependents`/`promote_ready`, `cascade_dependency_failed`/`reconcile_dependency_failed`, `propagate_closure_complete`/`reconcile_closure_complete`, `reconcile_missing_inputs`/`demote_unbacked_trusted_outputs`.

**`edges_complete` is the one un-hardened gating flag** and a live stale-comment hazard: nothing ever writes `edges_complete = false` (grep confirms only `= true` at `promotion.rs:456,572`; `demote_cached_output` leaves it intact `cache_storage.rs:274-278`), yet `promotion.rs:434-437` and `build.rs:1031` describe "a prior demote that cleared edges_complete." If someone trusts the comments they misunderstand the invariant; if they "fix" demote per the comments they reintroduce a dead zone.

## Messiness & code smells (ranked)

1. **Healing orchestration is duplicated across three call-sites with subtly divergent order and membership** (`dispatch.rs:373-460`, `eval.rs:905-969`, `build.rs:1021-1082`). No two are identical; the sequences are ordering-sensitive (CLEAR before SET, demote before reconcile-cached before promote) yet re-derived by hand at each site. Every new dead zone has meant "add one more call to some subset of these three lists."
2. **The dependency-readiness predicate is copy-pasted four times in raw SQL**: `promote_dependents` (`promotion.rs:79-95`), `promote_ready` (`:403-419`), the dispatch gate (`dispatch.rs:1038-1066`), and `CLOSURE_COMPLETE_GATE` (`:110-122`). A drift between any two is a latent dead zone.
3. **Raw SQL sprawl with hard-coded numeric status literals** (`promotion.rs:22-23` legend; "status = 6", "IN (3,7)", "IN (4,6,9)" throughout), guarded only by `sql.contains(...)` string-assertion tests. A renumbered enum silently corrupts every sweep. (Schema root cause in AUDIT-DB.md smell 1.)
4. **String-interpolated SQL for values that should be bound/typed** (`dep_closure.rs:92,112-131,196-221`, `build_attempt.rs:116-124`, `gc.rs:92`).
5. **`update_derivation_build_status` is a 150-line god-function mixing eight concerns** (`derivation_build_status.rs:24-175`), several as fire-and-forget `spawn`.
6. **Two parallel closure abstractions with overlapping walks** (`closure.rs`, `dependency_graph.rs`, `runtime_closure.rs`), plus the same recursive `derivation_dependency` walk re-expressed as inline SQL CTEs in at least five places.
7. **Stale/contradictory comments** describing behavior that no longer exists (the `edges_complete` clearing above).
8. **Asymmetric "bypass compensation" easy to forget**: bulk paths must manually re-emit CI status and re-finalize evals; `abort.rs:141` also manually calls `reconcile_eval_dep_counts`. Nothing enforces the pairing.
9. **Fire-and-forget error handling loses reconciliation failures.**

## Refactoring recommendations

1. **Introduce a single `reconcile_build_graph(db, storage, scope)` orchestrator** (the cross-cutting rec) that owns the canonical ordering and returns a typed `ReconcileReport` so callers do bypass-compensation uniformly. This is the highest-impact change and directly the "one legible flow" the project wants for self-heal.
2. **Define the build-graph invariants once**: one `const` (or query-builder fragment) for the readiness predicate and each gate, shared by promotion and dispatch so they can never disagree. Co-locate the flag-maintenance contract (which flag is monotonic vs bidirectional and why) as module docs next to the gates.
3. **Make `edges_complete` bidirectional like the others, or formally prove and document its monotonicity** and delete the contradictory comments.
4. **Replace bulk raw-SQL status literals with a typed transition layer** (`i32::from(BuildStatus::X)` or `BuildStatus::sql_set(&[...])`), and named set constants shared across sweeps.
5. **Extract one canonical `derivation_dependency` closure primitive** and reuse it in the CTEs (as `gc.rs` already does with `REACHABLE_DERIVATIONS_CTE`).
6. **Unify the demote/heal family under an explicit ownership model** ("storage reclaim", "anchor demotion", "flag reconciliation"), each doing one thing, composed by the orchestrator.
7. **Split `update_derivation_build_status` into transition-persist + a side-effect dispatcher** so reactive and bulk paths funnel fan-out through the same emitter (the reactive-side analogue of rec 1).
8. **Add a graph-consistency assertion sweep for observability**: a read-only tick check counting invariant violations (stale-true flags, Created anchors whose deps are all terminal-success, Completed anchors with unbacked outputs, evals Building with no non-terminal anchors) as a metric. Turns "eval stuck forever, found by a user" into an alert and validates that recs 1-3 actually eliminated the dead zones.

---

# 6. Garbage collection (the counterpart to the scheduler)

Scope: `gradient-db/src/gc.rs`, `gradient-cache/src/cacher/{mod,cleanup,deep_gc,sign_sweep,eval_cache_sweep,invalidate}.rs`, `gradient-db/src/{admin_tasks,cache_storage}.rs`. GC is the counterpart to the scheduler: both walk the build graph, both compute reachability/keep-sets, and GC races the pipeline the same way the scheduler dead-zones.

## Current flow

Five deletion surfaces plus one on-demand invalidator, all periodic loops spawned in `gradient-cache/src/lib.rs:14-16`:
- `cache_loop` (`mod.rs:36`, interval 3600s hardcoded): a sequential block of 9 sweeps (`cleanup_orphaned_cache_files`, `cleanup_old_evaluations` -> `gc_project_evaluations`, `gc_orphan_derivations`, `cleanup_stale_cached_nars`, `demote_unbacked_trusted_outputs`, `unpark_storage_full_all`, `cleanup_stale_build_request_blobs`, `cleanup_expired_upload_sessions`, `PartialStore::gc`).
- `sign_sweep_loop` (`mod.rs:125`, 60s hardcoded): `sign_missing_signatures` + `record_newly_completed_derivations` (feeds the GC keep-set/TTL).
- `eval_cache_sweep_loop` (`eval_cache_sweep.rs:122`, configurable): `evict_eval_cache` (age then size).
- `run_deep_gc` (admin task): `pass_nars` re-calls `cleanup_orphaned_cache_files`, `pass_blobs`, `pass_logs`.
- `invalidate_cache_for_path` (API-triggered).

Each sweep computes its keep-set / reachability differently: `gc_orphan_derivations` uses a raw recursive CTE (`gc.rs:188` `REACHABLE_DERIVATIONS_CTE`); `cleanup_orphaned_cache_files` uses a flat 4-way UNION (`cleanup.rs:367` `ACTIVE_HASHES_SELECT`); `cleanup_stale_cached_nars` uses EXISTS subqueries (`cleanup.rs:134`); `gc_project_evaluations` uses index math; `evict_eval_cache` is a pure fn. Meanwhile the scheduler expresses the same reachability in Rust BFS forward/reverse, single-level refcount, runtime walk, and SQL CTEs in promotion. **There are at least seven implementations of "walk `derivation_dependency`."**

## GC to build-graph races & correctness hazards (the mirror of the dead-zone class)

1. **Zombie `cached_path` - deleted row that a stale monotonic flag still trusts (marquee hazard).** GC deletes a NAR (`purge_zombie_cached_paths` `cleanup.rs:318`, `cleanup_stale_cached_nars` `:149`, `gc_orphan_derivations` `gc.rs:346`) but `drv_closure_cached`/`closure_complete` stay true, so the dispatch gate dispatches a build whose closure is not actually cached -> terminal `InputsUnavailable`. Fix bolted on: the flags were made bidirectional and reconciled on the 5s tick, and `demote_unbacked_trusted_outputs` runs post-GC. Assessment: patchwork. GC deletion and flag repair are in different subsystems on different clocks (hourly GC vs 5s tick), so a live unsound window exists between them. Principled fix: GC deletion clears the affected anchors' flags in the same transaction.
2. **Upload race - reclaiming a freshly-uploaded NAR before its rows commit.** Fix bolted on: a grace window (`cleanup.rs:277-282`, `gc.rs:203`). Assessment: patchwork and knob-overloaded - `keep_orphan_derivations_hours` now means two unrelated things ("grace before deleting an orphan derivation row" and "don't reclaim a NAR younger than this"), and a 24h blanket window to cover a seconds-long upload also delays reclaiming genuine garbage by 24h.
3. **`gc_orphan_derivations` delete-vs-reclaim TOCTOU** (`gc.rs:242-343`): candidate output hashes snapshotted before the DELETE, reclaim after; guarded by `RETURNING` re-check and survivor queries, but a narrow unguarded window remains between the survivor queries and `nar_storage.delete`.
4. **Keep-set divergence between the two NAR reclaimers**: flat UNION (`cleanup.rs:367`, no closure) vs recursive CTE (`gc.rs:188`, full closure). Safe only by an informal "orphan-files pass is a safety net only" comment (`cleanup.rs:405-408`); drifts the moment either SQL is edited.
5. **GC disabled by scheduler dead zones**: `evaluations_to_gc` returns nothing if any eval in the project is active (`gc.rs:155`), and `cleanup_stale_cached_nars` excludes derivations with any non-failed build (`cleanup.rs:138-142`). A single stuck-Building eval freezes all evaluation GC and NAR TTL eviction for that project indefinitely, so a scheduler bug silently becomes an unbounded-storage bug.
6. **`invalidate_cache_for_path` leaves the producer trusted** (`invalidate.rs:41-89`): clears output cache + deletes NAR + revokes `cache_derivation`, but never resets the producing `derivation_build` status, manufacturing the "unbacked trusted output" dead zone that `demote_unbacked_trusted_outputs` exists to clean up. No transaction wraps the multi-step delete.
7. **Error-swallowing that can delete live data**: `cleanup_stale_cached_nars` is riddled with `.unwrap_or_default()`; the critical `still_held` at `cleanup.rs:241` defaults to `false` on a DB error, so a transient error proceeds to delete a NAR that may still be referenced.

## Messiness & code smells (ranked)

1. Seven reimplementations of build-graph reachability; GC's is a raw-SQL fork of the scheduler's Rust BFS with a different root set. Single biggest structural smell; direct cause of hazard 4.
2. Three divergent keep-set definitions (`ACTIVE_HASHES_SELECT`, `REACHABLE_DERIVATIONS_CTE`, `STALE_CACHED_NARS_SELECT`); "still needed" defined three ways in one crate.
3. `cleanup.rs` is a 902-line god-file (8 sweeps + 3 raw-SQL constants); the full `ServerState { .. }` test literal is duplicated 6+ times (`:474,601,718,756,836`, `deep_gc.rs:205`).
4. `cleanup_stale_cached_nars` (`:149-258`) is a 110-line N+1 god-function with a hand-rolled cascade the schema already does, plus the error-swallowing of hazard 7.
5. `purge_zombie_cached_paths` loads the entire `cached_path` table into memory (`:322-326`) then filters in Rust, "hundreds of thousands of rows" every hour. Should be a set-based SQL anti-join.
6. Monolithic sequential `cache_loop` (`mod.rs:59-119`): 10 sweeps on one hourly timer, no independent intervals, no per-sweep isolation or metrics; `deep_gc` re-runs sweep #1.
7. Magic constants scattered (`3600`, `60`, `SIGN_SWEEP_BATCH=1000`, `ZOMBIE_DELETE_BATCH=8000`, `STORAGE_HEADROOM_BYTES=10 MiB`, grace `hours*3600`); two of three loop intervals hardcoded, one configurable.
8. `record_newly_completed_derivations` (`sign_sweep.rs:183`) is O(orgs x derivations) with per-derivation N+1.
9. Silent serialization swallow (`deep_gc.rs:37`).
10. The one clean sweep is `eval_cache_sweep` (pure `select_evictions`, 6 unit tests, own interval, error-isolated) - the positive baseline the others should converge toward.

## Refactoring recommendations

The through-line: GC and the scheduler are two walkers of one graph. Unify the walk, unify the keep-set, and make deletion maintain invariants inline instead of repairing them on a lag.

1. **One build-graph reachability primitive shared by GC and scheduler** (the cross-cutting rec): merge forward/reverse BFS into `reachable(db, roots, Direction)`, express the keep-set closure as one named `build_closure_cte(roots_subquery)` reused by `gc_orphan_derivations`, `mark_edges_complete_for_eval`, and `cascade_dependency_failed`. Makes hazard 4 impossible by construction.
2. **Extract a single `KeepSet`/`live_nar_hashes(db)`** used by every NAR reclaimer, collapsing the three divergent selects into derivations of one closure.
3. **Make GC deletion maintain the dispatch-gate invariant inline** (fixes the marquee race for real): when GC deletes a `cached_path`/NAR, clear `closure_complete`/`drv_closure_cached` on affected anchors and demote unbacked producers in the same transaction. Strategic alternative: model those flags as computed (VIEW/gate-on-read) so GC can never make them stale, the "how it should have been done" answer that also resolves AUDIT-DB.md smell 3.
4. **A principled upload lease instead of the 24h grace window**: record a short-lived `nar_lease(hash, expires_at)` on upload start; the keep-set includes un-expired leases. Bounds the un-reclaimable window to the actual upload duration and splits the two overloaded meanings of `keep_orphan_derivations_hours` into two honest knobs (update docs + nix modules per project rule).
5. **Unify sweep scheduling into a registry** mirroring the scheduler's loops: a `Sweep` trait `{ name, interval, run }` with per-sweep error isolation + metrics; `deep_gc` runs the same registry once synchronously with progress. Make the hardcoded intervals configurable. `eval_cache_sweep` is the template.
6. **Convert per-row cascades and load-all-filter loops to set-based SQL** (`cleanup_stale_cached_nars` FK cascade, `purge_zombie_cached_paths` anti-join).
7. **Stop defaulting-to-delete on failed reference checks**: propagate errors from `still_held`/reference queries; a row whose check errored must be skipped, never reclaimed.
8. **Decouple GC liveness from scheduler liveness**: replace the coarse "any active eval -> skip all GC" gate with a per-eval reachability check plus a max-age escape hatch, so a wedged Building eval can no longer disable GC.
9. **Kill the duplicated `ServerState` test literal** (extract a `test_server_state(overrides)` builder, see AUDIT-TEST.md).

Priority: recs 1-3 are the correctness core (unify the walk, unify the keep-set, make deletion invariant-preserving) and retire the race class; 4-5 make the subsystem legible; 6-9 are hygiene with fleet-scale performance and safety upside.
