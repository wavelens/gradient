# Scheduler Scoring

When a worker requests work, the scheduler ranks every eligible queued job for
that worker and offers the highest scorer. Scoring is pluggable: the `score`
crate (`backend/score`) defines a `ScoringPolicy` trait and a set of composable
rules, and the active policy is selected at startup.

## Selecting a policy

Set the server option `settings.schedulerScoringPolicy` (env
`GRADIENT_SCHEDULER_SCORING_POLICY`, clap field
`EvalArgs.scheduler_scoring_policy`). Values: `simple` (the basic rule set) and
`resource-aware`. `resource-aware` is the default selected when the env var is
unset, and unknown names log a warning and fall back to `resource-aware`.
`policy_by_name` (`backend/score/src/policy.rs`) resolves the string to an
`Arc<dyn ScoringPolicy>`.

## Architecture

- `ScoringPolicy` (`policy.rs`): `name()` plus `score(job, worker) -> f64`.
- `RulePolicy`: a named `Vec<Box<dyn ScoreRule>>` whose `score` sums each rule.
- `ScoreRule` (`rule.rs`): one `score(&JobContext, &WorkerContext) -> f64`
  contribution. Positive values raise priority, negative lower it.
- `JobContext` carries the candidate's missing-path count, missing NAR size,
  dependency count, queue time and the owning org's active-build share.
  `WorkerContext` carries the worker's architectures, system features, fetch
  capability and optional live `WorkerMetricsView` (free RAM, CPU-core score,
  disk speed, network speed).
- `ScoredJob` exposes lazy providers (`closure_size`, `history`) so a policy
  pays for closure/history lookups only when a rule reads them.

The scheduler builds the contexts, calls the configured policy per candidate,
and assigns the worker its top-scoring job.

## `simple` policy rules

| Rule | Effect |
|---|---|
| `MissingPathsRule` | Bonus when path availability is known (worker can serve the closure), minus a per-missing-path penalty. |
| `MissingNarSizeRule` | Penalty proportional to the MiB the worker would have to fetch. |
| `DependencyCountRule` | Small bonus per dependency for build jobs (unblocks more downstream work first). |
| `WaitTimeRule` | Bonus growing with queue wait, capped at one hour, for anti-starvation. |
| `BuiltinDeprioritizeRule` | Penalty for `builtin`-architecture build jobs. |
| `ReserveFetchWorkersRule` | Penalty when a fetch-capable worker is offered a cached-eval job, steering it toward fetch work. |

## `resource-aware` policy rules

Adds the following on top of the `simple` rule set:

| Rule | Effect |
|---|---|
| `ResourceFitRule` | Uses the job's history prediction and the worker's live metrics: penalty scaling with predicted-RAM overshoot of free RAM (amplified by past OOM rate), bonus for CPU-heavy jobs on higher-CPU-score workers. No-op without history samples or worker metrics. |
| `PreferLocalBuildRule` | Bonus for `preferLocalBuild` derivations on a worker that already holds (most of) the closure, decaying with missing paths. |
| `FairShareRule` | Penalty proportional to the owning org's share of currently-active builds, so a quiet org is served promptly when a busy org floods the queue. |
| `NetworkAffinityRule` | Bonus for fixed-output derivations (which fetch from the network) on faster-network workers, scaling to a reference speed then capping. No-op for non-FOD jobs or without a network metric. |
| `DiskAffinityRule` | Bonus for disk-heavy jobs (by history disk bytes) on faster-disk workers, scaling to a reference speed then capping. No-op below the disk-heavy threshold or without a disk metric. |

## Worker speed signals

The worker measures both speeds passively and reports them on the 10 s
`WorkerMetrics` heartbeat as exponentially-weighted moving averages:

- **Network speed (Mbps):** derived from real NAR transfers (upload in
  `push_direct`, download in `NarReceiver::accept_chunk`) — bytes over elapsed
  transfer time. Fixed-output derivations are flagged at parse time
  (`is_fixed_output`, persisted on the `derivation` row) and steered toward
  faster-network workers.
- **Disk speed (MB/s):** derived from per-build cgroup `io.stat`
  (`disk_read_bytes + disk_write_bytes`) over build wall-time. The wall-time
  divisor underestimates peak throughput for CPU-bound builds, but across many
  builds it reliably separates tmpfs-RAM / SSD / HDD `/tmp` build dirs. Per
  derivation, `HistoryPrediction.avg_disk_bytes` estimates disk dependence.

Both stay `None` until the first NAR transfer / build, in which case the
affinity rules are no-ops.

## Adding a rule or policy

Implement `ScoreRule` for a new contribution, add it to a rule list in
`policy.rs` (or a new list), and register the named policy in `policy_by_name`.
Each rule has unit tests in `backend/score/src/rules`; run them with
`cargo test -p score`.
