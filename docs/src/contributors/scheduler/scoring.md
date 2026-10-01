# Scoring

The scheduler ranks every eligible pending job for the requesting worker with a `ScoringPolicy` and assigns the top candidate. Scoring lives in `backend/gradient-pool/src/score`; the scheduler in `backend/gradient-scheduler` fills the inputs. The rule list and magnitudes are in [Scheduler Policies](../../reference/scheduler-policies.md).

```mermaid
flowchart LR
    W[Worker offer scores] --> J[JobContext]
    D["Assignment pass: history, closure size"] --> J
    H[Heartbeat metrics] --> K[WorkerContext]
    I[instance-metrics pass] --> C[InstanceContext]
    J & K & C --> P[RulePolicy::score_detailed]
    P --> A{wins}
    A --> X[Assignment]
```

## Traits

| Item | File | Role |
|---|---|---|
| `ScoringPolicy` | `policy.rs` | `name`, `score`, `score_detailed`, `uses_history`, `uses_project_work_share` |
| `RulePolicy` | `policy.rs` | Named `Vec<Box<dyn ScoreRule>>`; `score` sums the rules, `score_detailed` also collects per-rule scores and vetoes |
| `ScoreRule` | `rule.rs` | `name` (persisted key), `score`, `veto`, `uses_project_work_share`, `description` |
| `ScoreBreakdown` | `breakdown.rs` | `rules`, `total`, `vetoes`; stored in `dispatched_job.score_breakdown` |
| Weights | `weights.rs` | Every rule magnitude and `ASSIGN_FLOOR` in one file |

- `policy_by_name` maps `scheduler.scoringPolicy` to a `RulePolicy`; an unknown name logs a warning and uses `resource-aware`.
- Policies are declarative tables (`simple_table`, `resource_aware_table`) of `spec(enabled, rule)` rows. `FairShareRule` sits there with `enabled = false`.
- `uses_project_work_share` is derived from the enabled rules; `uses_history` is a constructor flag of `RulePolicy::new`.

## Contexts

| Context | Fields | Filled by |
|---|---|---|
| `JobContext` | `ScoredJob` (kind, architecture, `prefer_local_build`, `is_fixed_output`, `pname`, closure size, history), `missing_count`, `missing_nar_size`, `outputs_present`, `dependency_count`, `queued_at`, `ready_at`, `project_work_share`, `prioritized`, `rescore_count`, `now` | `JobTracker::score_candidates` in `gradient-scheduler/src/jobs.rs` |
| `WorkerContext` | `architectures`, `system_features`, `fetch`, `metrics` | `worker_context_of` from the worker's `WorkerCaps` |
| `WorkerMetricsView` | `cpu_count`, `cpu_core_score`, `ram_total_mb`, `ram_free_mb`, `cpu_usage_pct`, `disk_speed_mbps`, `network_speed_mbps` | `WorkerCapabilities` (static) and the 10 s `WorkerMetrics` heartbeat (live) |
| `InstanceContext` | 13 `Windowed` averages, `active_builds`, `pending_builds`, `total_workers`, `idle_workers`, `cpu_core_score_mean` | `instance_metrics_pass`, see below |

- `missing_count`, `missing_nar_size` and `outputs_present` are per worker: the worker scores each offered candidate against its store and sends a `CandidateScore` (see [Offers](../proto/capabilities-and-dispatch.md#offers)). `None` until that worker reported.
- `dependency_count` is the number of direct input derivations (`derivation_dependency` rows), not the number of builds that need the derivation.
- `ready_at` is when the dependencies finished; `WaitTimeRule` measures from there, not from `queued_at`.
- `rescore_count` grows by one per 5 s assignment timer tick (`BumpRescore`); reactive kicks leave the count unchanged.
- `now` is passed in: rules never read the wall clock.
- `JobContext::build_history` returns an empty prediction when `outputs_present`: a worker holding every output builds nothing.

## History and Closure Size

`ScoredJob` holds owned values; nothing is computed during scoring. `load_sizes_and_histories` in `loops/build.rs` materializes both on the pending job, and only when `policy.uses_history()`.

| Value | Source |
|---|---|
| Closure size | `derivation.closure_size`, else one batched `transitive_closure_sizes` walk; computed sizes are persisted by the graph writer |
| Build history | `history::predict`: latest 20 `derivation_metric` rows with the same `history_name` (`pname`, else `name`) and architecture, within [`retentionDays`](../../reference/configuration.md#general); one query per distinct pair |
| Evaluation history | `compute_eval_history`: per-task p95 of `evaluation_metric.peak_rss_mb` over 24 h |

`HistoryPrediction` carries the p95 peak RAM, mean CPU time, mean build time, mean disk bytes, OOM rate and `samples`. Rules treat `samples == 0` as no history and add nothing.

## Instance Windows

`instance_metrics_pass` is running every `metrics.instanceIntervalSecs` (30 s) and publishes the snapshot through an `ArcSwap`.

| Source table | Windows |
|---|---|
| `derivation_metric` | `peak_ram_mb`, `cpu_time_ms`, `avg_cpu_pct`, `disk_bytes`, `network_mbps`, `build_time_ms`, `closure_size`, `oom_rate`, `completed` |
| `dispatched_job` (builds with `ready_at`) | `wait_secs`, `nar_size_mb`, `missing_paths`, `dependency_cnt` |

- Each `Windowed` holds 5 min, 1 h and 24 h averages. `None` means no samples; a measured zero stays zero.
- Rules read `w1h_or(fallback)` or `w24h_or(fallback)` for instance-relative thresholds.
- A failed query leaves its windows empty; the in-memory counts always survive.

## Assignment Floor and Vetoes

- Candidates are sorted by `total`, ties broken by the smaller job id.
- The first candidate that passes `wins` is assigned: not vetoed and `total >= ASSIGN_FLOOR` (0.0). A vetoed higher candidate stays pending. Without a passing candidate the worker idles this round.
- A veto is a hold independent of the sum: large bonuses cannot outvote the veto. `RescoreWaitRule` is the only vetoing rule: score 0, and a hold on a build while `missing_nar_size` is `None` and `rescore_count < 4`.
- Every decision, rejected candidates included, goes into a 200-entry ring for the [Job Board](../../ui/job-board.md#job-inspection).

## Worker Speed Signals

| Signal | Measured in | Formula |
|---|---|---|
| `network_speed_mbps` | Passthrough NAR upload (`nar.rs`), NAR receive (`nar_recv.rs`), presigned PUT (`object_put.rs`) and presigned download (`download_one_presigned`) | bits / elapsed seconds / 10^6 |
| `disk_speed_mbps` | `build_metrics.rs` after each build | cgroup `disk_read_bytes + disk_write_bytes` in MiB / build seconds |
| `cpu_core_score` | Startup micro-benchmark, or `GRADIENT_WORKER_SYSTEM_CPU_CORE_SCORE` | Static, sent with `WorkerCapabilities` |

- Network and disk are EWMAs (`alpha = 0.3`) in `gradient-worker-client/src/throughput.rs`, `None` until the first sample.
- `cpu_core_score_mean` is the mean over connected workers with a non-zero score.

## Adding a Rule

1. Add the magnitudes to `weights.rs`.
2. Implement `ScoreRule` in `rules/<area>.rs` with a stable `name` and a `description`; override `veto` for a hold, `uses_project_work_share` when reading the share.
3. Re-export the rule in `rules/mod.rs` and add a `spec(true, ...)` row to a policy table in `policy.rs`.
4. A new policy also needs a `policy_by_name` arm, `uses_history = true` when a rule reads history, and the enum value in `nix/modules/gradient.nix`.
5. Unit tests sit next to the rule in `#[cfg(test)] mod tests`.

`rule_catalog` (`GET /board/scoring/rules`) lists every rule of the `resource-aware` table, disabled ones included, for the board to explain any rule a recorded breakdown names.

## Related

- [Scheduler Policies](../../reference/scheduler-policies.md): every rule and its magnitude
- [Capabilities and Assignment](../proto/capabilities-and-dispatch.md): offers, candidate scores and assignment
