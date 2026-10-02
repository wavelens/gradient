# Waiting and Recovery

The scheduler splitting a flake job across workers, parking an evaluation unable to progress, re-offering a returned job and cleaning up after a server restart.

```mermaid
flowchart LR
    P[Queued / Fetching / Evaluating*] -- no capable worker --> W[Waiting]
    B[Building] -- no worker fitting / graph stuck --> W
    W -- eval_workers, draining cleared --> P
    W -- workers fit / heal freeing a shared build --> B
```

## Fetch and Eval Split

`assign_queued_evals` (`gradient-scheduler/src/loops/eval.rs`) is asking the pool once per pass for an idle evaluation-only worker (`WorkerPool::has_idle_eval_only_worker`: active, `eval` without `fetch`, no assigned job).

| Pool at assignment | Job steps |
|---|---|
| An idle evaluation-only worker present | `[FetchFlake]`, then a follow-up `[EvaluateFlake, EvaluateDerivations]` |
| Otherwise | `[FetchFlake, EvaluateFlake, EvaluateDerivations]` in one job |

- **Decision per pass:** one idle worker is splitting every evaluation queued in that pass.
- **Source:** a repository evaluation is carrying `FlakeSource::Repository`. A `/nix/store/...` source (`gradient build` upload) is carrying `FlakeSource::Cached`. That source is still running `FetchFlake` to resolve inputs with the project SSH key (`flake_job_for_eval_source`).
- **Routing:** `WorkerCaps::can_eval` (`gradient-pool/src/worker_caps.rs`) is requiring `fetch` for a `FetchFlake` step and `eval` for an evaluation step.
- **Follow-up:** `handle_job_completed` (`job_handlers/build_status.rs`) is reacting to `JobCompleted` of a fetch-only job (`is_fetch_only`). The handler is reading `evaluation.flake_source` and enqueueing `PendingEvalJob::cached_followup` under the same `eval:{id}` key. The follow-up is carrying `FlakeSource::Cached` plus the source as a required path. A missing `flake_source` is failing the evaluation permanently.
- **Worker side:** the evaluating worker is substituting the source from the cache and evaluating `path:<store path>` (`gradient-worker/src/executor/eval.rs`).
- **Steering:** `ReserveFetchWorkersRule` (`gradient-pool/src/score/rules/builtin.rs`) is scoring a fetch-capable worker `-300 * (1 - idle / total)` for a job without `FetchFlake`. The penalty is fading as workers go idle. The rule is never refusing the job. See [Scoring](scoring.md).

## Waiting Reasons

The waiting pass, `refresh_waiting_state` (`gradient-scheduler/src/waiting_state.rs`), is running at the end of every build assignment pass (5 s tick or kick) over every in-flight evaluation. The pass is writing the reason into `evaluation.waiting_reason` only on a change of its JSON.

| `WaitingReason` | Parked from | Unparked | Owner |
|---|---|---|---|
| `eval_workers { capability, connected_workers }` | `Fetching` without a `fetch` worker. `Queued`, `EvaluatingFlake`, `EvaluatingDerivation` without an `eval` worker | To `Queued` once the capability is connected | Waiting pass |
| `workers { unmet, connected_workers, available_architectures }` | `Building` with no worker fitting the `(architecture, required_features)` of any blocking shared build | To `Building` once one is fitting. `Aborted` after 300 s on the same `unmet` set, unless `wait_for_workers` is set on the task | Waiting pass |
| `graph_stuck { pending_shared_builds }` | `Building` with a fitting worker for every blocking shared build, yet none can be handed out | To `Building` once the heal is freeing a shared build | Waiting pass |
| `draining` | Every in-flight evaluation during an instance drain | To `Queued` at the end of draining or at startup (`unpark_draining_evals`) | Waiting pass |
| `approval`, `no_cache`, `cache_storage_full` | Trigger gates | Webhook and cache hooks (`gradient-ci/src/unpark.rs`) | Never touched by the waiting pass |

- **Pre-build parks** ignore builds the evaluation already batched. A stall mid-walk is still parking.
- **Blocking builds** are the named shared builds kept by `blocks_evaluation`, in a status that can still be wanted. A shared build wanted by nothing is left out.
- **Counters first:** `phase_from_counters` is deciding without reading a shared build. `named = 0` is handing the evaluation to the pre-build rules. `active = 0` is finalizing the evaluation through `check_evaluation_done`. `building > 0` is keeping `Building`.
- **Unbuildable abort:** `unbuildable` (`gradient-scheduler/src/unbuildable.rs`) is handing a `workers` park with a non-empty `unmet` set back to the scheduler. The hand-back is happening once the park is older than 300 s and the task is set to `wait_for_workers = false`. `abort_unbuildable_evaluation` is marking the evaluation `Aborted` and aborting its shared builds. The function is also recording a warning naming each missing architecture and feature set. The grace is keeping a server restart or worker redeploy from aborting work before the pool is back.
- **Shared build read:** `BuildabilityChecker` is reading the blocking shared builds otherwise. `AssessmentMemo` is reusing the verdict for up to 60 s while the counters and the pool fingerprint stay equal. An empty read is recounting the drifted counters and finalizing.

## Graph-Stuck Heal

A `workers` verdict with an empty `unmet` set is turning into the heal. `attempt_graph_unstick` is sending `Transition::Repair { scope: RepairScope::Unstick }` to the [graph writer](shared-builds.md#graph-writer). The function is then re-assessing without the memo. The [repair pass](repair-pass.md) `repair_build_graph` (`gradient-db/src/graph/repair.rs`) is running the steps below. The first failed step is failing the transition.

1. `requeue_failed_closure`: thawing failed shared builds in the closure to `Created`. `Unstick` is leaving a deterministic build failure and its subtree alone.
2. `repair_cached_shared_builds_for_eval`: completing shared builds with every output in the cache, then `advance_fetchable` for the builds needing them.
3. `repair_dependency_failed`: failing the builds needing a build with a lasting failure.
4. `adopt_pending_closure`: naming orphaned pending shared builds for the evaluation, then updating their needs-build marks.
5. `promote_closure`: promoting every build now passing the gates.

| Driver | When |
|---|---|
| Waiting pass | On entry to `graph_stuck` and on every change of `pending_shared_builds` (`unstick_due`) |
| `graph-stuck-reheal` pass | Every `metrics.graphConsistencyIntervalSecs` (300 s) for each `graph_stuck` evaluation |
| Counters | Promoting the set the moment its gates open |

- **Lost `.drv`:** `recover_drv_stuck_evals` is checking evaluations `graph_stuck` for over 120 s. Its target is a walked shared build needing a build, not available in a cache, with every dependency done and no `.drv` NAR. `trigger_drv_recovery` is starting a `DrvRecovery` evaluation of the same commit for such a build. A `DrvRecovery` evaluation stuck the same way is failing.

## Re-Offering Returned Jobs

Offers are deltas. The server is sending a worker only candidates missing from its `sent_candidates`. The worker is scoring every offered candidate. Each offer is resetting the set to the candidates currently visible to the worker. A claimed, finished or aborted job is dropping out of the set. Returning a job to the pool is clearing its sent flag.

| Event | Mechanism |
|---|---|
| Enqueue | `SchedulerMsg::Enqueue` calling `remove_sent_candidate` for every worker, also for the cached follow-up reusing `eval:{id}` |
| Reject | `SchedulerMsg::Rejected` returning the job to pending and clearing its sent flag |
| Assignment pass | `SchedulerMsg::ReOffer` bumping the offer generation while any job is pending. Sessions then pull the delta |

## Startup Recovery

`recover_interrupted_work` (`gradient-db/src/maintenance/recovery.rs`) is running once at server start (`gradient-web/src/lib.rs`), before `unpark_draining_evals`.

1. **Assignments:** every open `dispatched_job` row is closing as `Abandoned`. Both assignment selections refuse a job with an open row. Re-queued work would otherwise wait for its worker to reconnect or for the 1800 s check for abandoned assignments.
2. **Attempts:** `Running` build attempts turn `Aborted`.
3. **Shared builds:** every `Building` shared build is turning `Queued`. `unpromote_ungated` is pulling those with gates no longer passing back to `Created`.
4. **Evaluations:** every status in `EvaluationStatus::ACTIVE` except `Queued` and `Waiting` is turning `Aborted` with `finished_at`. `Building` is included. Nothing else is driving its remaining builds.
5. **Their shared builds:** `Created`, `Queued` and `Building` shared builds of those evaluations turn `Aborted`, unless a non-terminal evaluation is also naming them.
6. **Tasks:** each affected task is getting `force_evaluation`. The fresh evaluation is thawing the aborted shared builds through `RepairScope::Eval` once its stream is complete.
7. **Clusters:** every open [cluster attempt](clusters.md) is closing (`PrepareFailed` when unstarted, `Aborted` otherwise). A `Running` cluster is going back to `Queued`, or to `Aborted` when a member evaluation or shared build can no longer finish.

- `Queued` evaluations return through the eval assignment pass. `Waiting` ones through the waiting pass.
- Recovery is skipping the live effects of `update_evaluation_status`. The rows are consistent on their own.
