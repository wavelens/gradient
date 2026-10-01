# Waiting and Recovery

How the scheduler splits a flake job across workers, parks an evaluation that cannot progress, re-offers a returned job, and cleans up after a server restart.

```mermaid
flowchart LR
    P[Queued / Fetching / Evaluating*] -- no capable worker --> W[Waiting]
    B[Building] -- no worker fits / graph stuck --> W
    W -- eval_workers, draining cleared --> P
    W -- workers fit / heal frees a shared build --> B
```

## Fetch and Eval Split

`assign_queued_evals` (`gradient-scheduler/src/loops/eval.rs`) asks the pool once per pass for an idle evaluation-only worker (`WorkerPool::has_idle_eval_only_worker`: active, `eval` without `fetch`, no assigned job).

| Pool at assignment | Job steps |
|---|---|
| An idle evaluation-only worker exists | `[FetchFlake]`, then a follow-up `[EvaluateFlake, EvaluateDerivations]` |
| Otherwise | `[FetchFlake, EvaluateFlake, EvaluateDerivations]` in one job |

- **Decision per pass:** one idle worker splits every evaluation queued in that pass.
- **Source:** a repository evaluation carries `FlakeSource::Repository`; a `/nix/store/...` source (`gradient build` upload) carries `FlakeSource::Cached` and still takes the `FetchFlake` step to resolve inputs with the project SSH key (`flake_job_for_eval_source`).
- **Routing:** `WorkerCaps::can_eval` (`gradient-pool/src/worker_caps.rs`) requires `fetch` for a `FetchFlake` step and `eval` for an evaluation step.
- **Follow-up:** on `JobCompleted` of a fetch-only job (`is_fetch_only`), `handle_job_completed` (`job_handlers/build_status.rs`) reads `evaluation.flake_source` and enqueues `PendingEvalJob::cached_followup` under the same `eval:{id}` key: `FlakeSource::Cached` plus the source as a required path. A missing `flake_source` fails the evaluation permanently.
- **Worker side:** the evaluating worker substitutes the source from the cache and evaluates `path:<store path>` (`gradient-worker/src/executor/eval.rs`).
- **Steering:** `ReserveFetchWorkersRule` (`gradient-pool/src/score/rules/builtin.rs`) scores a fetch-capable worker `-300 * (1 - idle / total)` for a job without `FetchFlake`. The penalty fades as workers go idle; the job is never refused. See [Scoring](scoring.md).

## Waiting Reasons

The waiting pass, `refresh_waiting_state` (`gradient-scheduler/src/waiting_state.rs`), closes every build assignment pass (5 s tick or kick) and covers every in-flight evaluation. The reason lands in `evaluation.waiting_reason` only when its JSON changes.

| `WaitingReason` | Parked from | Unparked | Owner |
|---|---|---|---|
| `eval_workers { capability, connected_workers }` | `Fetching` without a `fetch` worker; `Queued`, `EvaluatingFlake`, `EvaluatingDerivation` without an `eval` worker | To `Queued` once the capability connects | Waiting pass |
| `workers { unmet, connected_workers, available_architectures }` | `Building` when no worker fits the `(architecture, required_features)` of any blocking shared build | To `Building` once one fits; `Aborted` after 300 s on the same `unmet` set unless the task sets `wait_for_workers` | Waiting pass |
| `graph_stuck { pending_shared_builds }` | `Building` when every blocking shared build fits a worker but none can be handed out | To `Building` when the heal frees a shared build | Waiting pass |
| `draining` | Every in-flight evaluation while the instance drains | To `Queued` when draining ends or at startup (`unpark_draining_evals`) | Waiting pass |
| `approval`, `no_cache`, `cache_storage_full` | Trigger gates | Webhook and cache hooks (`gradient-ci/src/unpark.rs`) | Never touched by the waiting pass |

- **Pre-build parks** ignore builds the evaluation already batched: a stall mid-walk still parks.
- **Blocking builds** are the named shared builds that `blocks_evaluation` keeps, in a status that can still be wanted; a shared build nothing wants is left out.
- **Counters first:** `phase_from_counters` decides without reading a shared build. `named = 0` hands the evaluation to the pre-build rules, `active = 0` finalizes the evaluation through `check_evaluation_done`, `building > 0` keeps `Building`.
- **Unbuildable abort:** `unbuildable` (`gradient-scheduler/src/unbuildable.rs`) hands a `workers` park with a non-empty `unmet` set back to the scheduler once the park is older than 300 s and the task has `wait_for_workers = false`. `abort_unbuildable_evaluation` marks the evaluation `Aborted`, records a warning naming each missing architecture and feature set, and aborts its shared builds. The grace keeps a server restart or worker redeploy from aborting work before the pool reconnects.
- **Shared build read:** otherwise `BuildabilityChecker` reads the blocking shared builds. `AssessmentMemo` reuses the verdict for up to 60 s while the counters and the pool fingerprint stay equal. An empty read recounts the drifted counters and finalizes.

## Graph-Stuck Heal

A `workers` verdict with an empty `unmet` set turns into the heal: `attempt_graph_unstick` sends `Transition::Repair { scope: RepairScope::Unstick }` to the [graph writer](shared-builds.md#graph-writer), then re-assesses without the memo. The [repair pass](repair-pass.md) `repair_build_graph` (`gradient-db/src/repair.rs`) takes these steps, and the first failed step fails the transition:

1. `requeue_failed_closure`: thaws failed shared builds in the closure to `Created`; `Unstick` leaves a deterministic build failure and its subtree alone.
2. `repair_cached_shared_builds_for_eval`: completes shared builds whose outputs are all in the cache, then `advance_fetchable` for the builds that need them.
3. `repair_dependency_failed`: fails the builds that need a build whose failure stays.
4. `adopt_pending_closure`: names orphaned pending shared builds for the evaluation, then updates their needs-build marks.
5. `promote_closure`: promotes whatever the gates now pass.

| Driver | When |
|---|---|
| Waiting pass | On entry to `graph_stuck` and whenever `pending_shared_builds` changes (`unstick_due`) |
| `graph-stuck-reheal` pass | Every `metrics.graphConsistencyIntervalSecs` (300 s) for each `graph_stuck` evaluation |
| Counters | Promote the set the moment its gates open |

- **Lost `.drv`:** `recover_drv_stuck_evals` checks evaluations `graph_stuck` for over 120 s. When a walked shared build that needs building, is not available in a cache and has every dependency done lacks its `.drv` NAR, `trigger_drv_recovery` starts a `DrvRecovery` evaluation of the same commit. A `DrvRecovery` evaluation stuck the same way fails.

## Re-Offering Returned Jobs

Offers are deltas: the server sends a worker only candidates missing from its `sent_candidates`. The worker scores every offered candidate. Each offer resets the set to the candidates the worker can currently see, and a claimed, finished or aborted job drops out of the set. A job returned to the pool has its sent flag cleared.

| Event | Mechanism |
|---|---|
| Enqueue | `SchedulerMsg::Enqueue` calls `remove_sent_candidate` for every worker, also for the cached follow-up that reuses `eval:{id}` |
| Reject | `SchedulerMsg::Rejected` returns the job to pending and clears its sent flag |
| Assignment pass | `SchedulerMsg::ReOffer` bumps the offer generation while any job is pending; sessions then pull the delta |

## Startup Recovery

`recover_interrupted_work` (`gradient-db/src/recovery.rs`) takes place once at server start (`gradient-web/src/lib.rs`), before `unpark_draining_evals`.

1. **Assignments:** every open `dispatched_job` row closes as `Abandoned`. Both assignment selections refuse a job with an open row; without this step re-queued work waits for its worker to reconnect or the 1800 s check for abandoned assignments.
2. **Attempts:** `Running` build attempts turn `Aborted`.
3. **Shared builds:** every `Building` shared build turns `Queued`; `unpromote_ungated` pulls those whose gates no longer pass back to `Created`.
4. **Evaluations:** every status in `EvaluationStatus::ACTIVE` except `Queued` and `Waiting` turns `Aborted` with `finished_at`. `Building` is included: nothing else drives its remaining builds.
5. **Their shared builds:** `Created`, `Queued` and `Building` shared builds of those evaluations turn `Aborted`, unless a non-terminal evaluation also names them.
6. **Tasks:** each affected task gets `force_evaluation`. The fresh evaluation thaws the aborted shared builds through `RepairScope::Eval` once its stream completes.
7. **Clusters:** every open [cluster attempt](clusters.md) closes (`PrepareFailed` when unstarted, `Aborted` otherwise). A `Running` cluster goes back to `Queued`, or to `Aborted` when a member evaluation or shared build can no longer finish.

- `Queued` evaluations return through the eval assignment pass; `Waiting` ones through the waiting pass.
- The live effects of `update_evaluation_status` are skipped; the rows are consistent on their own.
