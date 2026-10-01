# Repair Pass

The repair pass heals graph state no event reaches; one emitter carries the consequences of every shared build's status move. Both live inside the graph writer; background passes in the scheduler re-drive what a lost message left behind.

```mermaid
flowchart LR
    T[Transition] --> A[Graph writer]
    A --> R[repair_build_graph]
    A --> E[emit_transition_effects]
    R --> E
    E --> O[(pending deliveries)]
    O --> F[Effects actor]
    E --> Q[StartableSet] --> D[Build assigner]
```

## Repair Scopes

`gradient_db::repair_build_graph(ctx, scope)` (`backend/gradient-db/src/repair.rs`) takes place as `Transition::Repair` in the graph writer: a repair pass never interleaves with a batch import. Both scopes name an evaluation; every statement is bounded to that evaluation's dependency closure.

| Scope | Sent by | Thaw |
|---|---|---|
| `Eval(id)` | `EvalStreamCompleted`; a `restart_failed` evaluation that starts `Building` (`POST .../evaluate`) | Every `REQUEUEABLE` shared build in the closure, a reproducible builder exit included |
| `Unstick(id)` | A `Building` evaluation judged graph-stuck (`waiting_state::attempt_graph_unstick`), and the `graph-stuck-reheal` pass | Same, minus the `deterministic_build_failure` subtree |

- A failure is valid for the evaluation that recorded the failure: a fresh evaluation rebuilds the shared build once, an unstick of the same evaluation leaves the shared build alone.
- `restart_failed` never walks; `inherit_names` copies the previous evaluation's `build_job` rows and the `Eval` heal operates on them.
- No tick starts a repair pass and no step iterates to a fixpoint.

## Repair Steps

All steps share the graph writer's transaction. The first failed step ends the pass and fails the transition: the graph writer rolls back and retries a deadlock or serialization failure.

| # | Function | Effect |
|---|---|---|
| 1 | `promotion::requeue_failed_closure` | Resets `FailedPermanent`, `Aborted`, `DependencyFailed`, `FailedTimeout` shared builds to `Created`, `attempt = 0` |
| 2 | `promotion::repair_cached_shared_builds_for_eval`, then `can_start::advance_fetchable` | Shared builds whose outputs are all in the cache turn `Completed`; the counters of the builds that need them advance |
| 3 | `promotion::repair_dependency_failed` | Fails every non-terminal shared build reachable from a failure that stays |
| 4 | `reachability::adopt_pending_closure` | Inserts `build_job` rows for open shared builds the evaluation reaches and nobody names; updates and settles the needs-build marks; bumps `graph_version` |
| 5 | `can_start::promote_closure` | Promotes the closure to `Queued` |

- Cache presence is the ground truth for "built" in step 2.
- Every step feeds its `TransitionChange`s to `emit_transition_effects`.

## Transition Effects

Both mutation models report `(derivation, from, to)` moves to `emit_transition_effects` (`backend/gradient-db/src/status/effects.rs`): the single-row path `update_derivation_build_status` and the bulk passes (promotion, cascades, repairs, abort, GC). No shared build moves without its consequences.

| Effect | Condition |
|---|---|
| `StartableSet::record` | Shared build entered or left `Queued` |
| `bump_graph_version` | Once per emit, for every evaluation with a `build_job` on a moved shared build (`from != to`) |
| Board `build::StatusChanged` | Every `build_job` of a moved shared build |
| `build::Reported` event, written as an `Event` pending-delivery row | Entry-point `build_job` moving to `Queued`, `Building` or a terminal status |
| `cache::Changed` | Any move into `Completed` or `Substituted` |
| `check_evaluation_done` | Every evaluation of a shared build that turned terminal |
| `LogFinalize` pending-delivery row | Latest attempt of every shared build that really finished |
| Needs-build update, queue settle, upstream probe request | Shared build crossed the `BUILDER_STATUSES` boundary; regated shared builds are announced in a second round |

- Pending-delivery rows (`pending_delivery`) are written in the transaction that moved the shared build. The `gradient-effects` actor expands `Event` rows into Git host, action and webhook deliveries and sends them with retries.
- Moves inside a graph transaction are staged on the `StartableSet` and published after the commit: the assigner reads on its own connection.
- `collapse_transitions` reduces two moves of one shared build in one transaction to the net move.

## Task Page Histogram

`evaluation.graph_version` invalidates the per-entry-point status histogram (`backend/gradient-db/src/dep_counts.rs`).

| Bumped by | Scope |
|---|---|
| `emit_transition_effects` | Evaluations of every moved shared build |
| Imported batch | Its own evaluation, plus every evaluation holding a derivation whose edge set grew |
| Adoption (repair pass, consistency check, evaluation GC) | The adopting evaluations |
| Startup recovery | Evaluations of re-queued and aborted shared builds (recovery has no emitter) |

- `task_board::entry_point_dep_counts` walks one page of entry points; rows land in `entry_point_dep_count` with `entry_point.dep_counts_version` and `dep_counts_computed_at`.
- A read refreshes an entry point only when the version moved and the rows are older than `DEP_COUNTS_REFRESH_SECS` (120 s).
- Rows older than `DEP_COUNTS_MAX_AGE_SECS` (600 s) refresh regardless; this heals a bump the emitter logged and swallowed.

## Consistency Check

`graph_consistency_report` (`backend/gradient-db/src/consistency.rs`) starts every `metrics.graphConsistencyIntervalSecs` (300 s, `GRADIENT_METRICS_GRAPH_CONSISTENCY_INTERVAL_SECS`). The counters are moved, never derived: this check is their only backstop. Each step completes before the one that reads its column.

| # | Step | Report field |
|---|---|---|
| 1 | Recount `unwalked_inputs` | `walk_drift` |
| 2 | Recount `missing_runtime_deps`, table-wide | `runtime_drift` |
| 3 | Recount `fetchable` over the repair scope | `counter_drift` |
| 4 | Recount `wanted`, table-wide | `need_drift` |
| 5 | `settle_skipped`: settle shared builds no evaluation needs to `Skipped`, thaw needed ones | `skipped_moves` |
| 6 | Recount `blocking_deps` over the repair scope, then un-promote and promote | `counter_drift`, `unpromoted_startable` |
| 7 | Adopt pending shared builds a live evaluation reaches and nobody names | `adopted` |
| 8 | Count unbacked outputs of terminal-success producers (read-only) | `unbacked_trusted_outputs` |
| 9 | Recount evaluation shared-build counters | `eval_counter_drift` |
| 10 | Count `Building` evaluations with `active_shared_builds = 0` (read-only) | `wedged_building_evals` |

- **Repair scope:** pending shared builds, their direct dependencies, `fetchable` rows with `missing_runtime_deps > 0`, and terminal-success rows without `fetchable`. Its size is logged as `scope` on every pass.
- Each chunk of 3 and 6 is its own transaction: `lock_shared_builds` (ordered `FOR NO KEY UPDATE`) first, then the recount. A cancelled check loses one chunk.
- A recount reads one snapshot and ripples nothing: a chain of drifted rows converges one level per interval.
- Drift counts are rows already repaired; a warning naming only those is a successful self-repair.
- Interval `0` disables the check and the `graph-stuck-reheal` pass.

## Watchdogs

Scheduler passes in `backend/gradient-scheduler/src/loops/background.rs`. A job the scheduler tracker still holds is never touched.

| Pass | Every | Finds | Action |
|---|---|---|---|
| `eval-completion-watchdog` | 60 s | Evaluation in `EvaluatingFlake`/`EvaluatingDerivation` whose newest eval assignment closed `Completed`/`Failed`, unwritten for 900 s | Re-sends `EvalStreamCompleted` or `EvalFailed` |
| `stranded-build-sweep` | 60 s | `Building` shared build whose newest attempt's assignment closed `Abandoned`, unwritten for 900 s | Sends `OrphanedBuilds` (moves only rows still `Building`) |
| `abandoned-dispatch-sweep` | 60 s | Open `dispatched_job` row older than 1800 s | Closes the row `Abandoned`; first reaps aborts unconfirmed after 300 s |
| `graph-stuck-reheal` | Consistency check interval | `Waiting` evaluations with reason `GraphStuck` | Sends `Repair { Unstick }` |

- The 900 s grace covers a transition still queued in the graph writer. A re-send that races that transition is harmless.
- Both re-sent transitions are idempotent.

## Assignment Record Closers

A `dispatched_job` row is the proof a job is out; `claim_assignment` inserts the row and `idx-dispatched_job-open-job` admits one open row per job key (`build:<shared_build>`, `eval:<evaluation>`).

| Closer | Outcome |
|---|---|
| Worker's terminal report, matched on the assignment ID | `Completed` / `Failed` |
| Claim whose `Assigned` transition fails or exceeds its budget (`assignment.rs`) | `Abandoned` |
| Worker rejects the assignment (`withdraw_assignment`) | `Abandoned` |
| Worker disconnect (`requeue_orphaned_jobs`) | `Abandoned` |
| Worker registration: rows assigned before this process started | `Abandoned` |
| Startup recovery (`abandon_all_open_assignments`) | `Abandoned` |
| Overdue abort reap, `abandoned-dispatch-sweep` | `Abandoned` |

- `Abandoned` is not `Failed`: the build may have succeeded before contact was lost, and a failure would skew the board's rates and history-based scoring.

## Missing Inputs

A build failing `InputsUnavailable` calls `self_heal::repair_missing_inputs` (`backend/gradient-graph/src/self_heal.rs`) per missing path:

1. `demote_cached_output`: clears `is_cached` and `external_url`, clears `cache_available`, retires the `cached_path` row, deletes the NAR.
2. A producerless path (`.drv`, source) is kept while its NAR still exists; the rebuildable outputs that reference the path are demoted instead (`demote_parents_of`).
3. An unreachable producer: the outputs that reference the path are demoted and un-walked; with no such output, the failing build's output-only-cached direct deps are demoted (`demote_output_only_cached_deps`).
4. Terminal-failed producers re-queue on the spot (`requeue_failed_shared_builds`).

- The failed build itself retries in-eval as `FailedTransient` with the transient backoff; the raised `blocking_deps` holds the build out of the queue until the input is back.
- After `build.inputsUnavailableMaxLoops` (3) prior `InputsUnavailable` attempts the self-heal is skipped and the build fails `Permanent`.

## Cache Deletions

`runtime_can_start::retire_outputs` is the one way a `cached_path` row goes, in the deleting transaction:

- Locks the `cached_path` rows in hash order, then the shared builds: a concurrent signature insert is waited out before the DELETE opens its snapshot.
- Raises `missing_runtime_deps` of every shared build that trusted the rows; the ripple takes the complete closure and `fetchable` from the builds that reference them.
- Resets to `Created` only producers whose artifact is actually gone; a referencing build that only lost its complete closure keeps its terminal status.
- Un-promotes the owners of a retired `.drv`.

| Caller | Path |
|---|---|
| Stale-path eviction, zombie purge | `GcRequest::Paths` -> `retire_stale_paths` in the graph writer |
| Missing inputs, path invalidation | `demote_cached_output` |

An unbacked terminal-success output is prevented, not repaired: a failed NAR commit answers `Retry` and fails the build, `JobCompleted` waits for every `UploadCommitted`, substitution requires every output with a complete closure, and every retire resets the producers of what the retire deleted. A non-zero `unbacked_trusted_outputs` is a bug report.

## Garbage Collection

| Pass | Reclaims |
|---|---|
| Evaluation GC (`gc_task_evaluations`, per task, keeps `keep_evaluations`) | Old evaluations; live evaluations adopt pending shared builds first (`adopt_pending_closures`) |
| Derivation GC (`run_derivation_gc`) | Derivations outside the dependency closure of every `entry_point` and `build_job`, after `gc.orphanDerivationHours` (24); their attempt logs are deleted from log storage by the same pass |
| Stale-path eviction (`evict_stale_cached_paths`) | Unreachable paths unfetched for `gc.narTtlHours` (336) |
| Orphan NAR files | Stored NARs no row keeps, probed in batches of 5000, older than `gc.narUploadGraceHours` (24) |

- `.drv` and input-source NARs of any derivation with a shared build are kept regardless of status: nothing but an evaluation re-pushes them.
- An active evaluation blocks its task's GC, unless its current phase is older than `gc.wedgedEvalHours` (24); the age comes from the phase stamps, not `updated_at`.
- Derivation GC re-checks roots created since its scan inside the graph writer; surviving derivations that need a deleted derivation are un-walked.
