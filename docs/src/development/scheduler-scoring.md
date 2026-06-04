# Scheduler Scoring

When a worker requests work, the scheduler ranks every eligible queued job for
that worker and offers the highest scorer. Scoring is pluggable: the `score`
crate (`backend/score`) defines a `ScoringPolicy` trait and a set of composable
rules, and the active policy is selected at startup.

## Selecting a policy

Set the server option `settings.schedulerScoringPolicy` (env
`GRADIENT_SCHEDULER_SCORING_POLICY`, clap field
`EvalArgs.scheduler_scoring_policy`). Values: `default` (the standard policy)
and `resource-aware`. Unknown names log a warning and fall back to `default`.
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
  capability and optional live `WorkerMetricsView` (free RAM, CPU-core score).
- `ScoredJob` exposes lazy providers (`closure_size`, `history`) so a policy
  pays for closure/history lookups only when a rule reads them.

The scheduler builds the contexts, calls the configured policy per candidate,
and assigns the worker its top-scoring job.

## `default` policy rules

| Rule | Effect |
|---|---|
| `MissingPathsRule` | Bonus when path availability is known (worker can serve the closure), minus a per-missing-path penalty. |
| `MissingNarSizeRule` | Penalty proportional to the MiB the worker would have to fetch. |
| `DependencyCountRule` | Small bonus per dependency for build jobs (unblocks more downstream work first). |
| `WaitTimeRule` | Bonus growing with queue wait, capped at one hour, for anti-starvation. |
| `BuiltinDeprioritizeRule` | Penalty for `builtin`-architecture build jobs. |
| `ReserveFetchWorkersRule` | Penalty when a fetch-capable worker is offered a cached-eval job, steering it toward fetch work. |

## `resource-aware` policy rules

Adds the following on top of the `default` rule set:

| Rule | Effect |
|---|---|
| `ResourceFitRule` | Uses the job's history prediction and the worker's live metrics: penalty scaling with predicted-RAM overshoot of free RAM (amplified by past OOM rate), bonus for CPU-heavy jobs on higher-CPU-score workers. No-op without history samples or worker metrics. |
| `PreferLocalBuildRule` | Bonus for `preferLocalBuild` derivations on a worker that already holds (most of) the closure, decaying with missing paths. |
| `FairShareRule` | Penalty proportional to the owning org's share of currently-active builds, so a quiet org is served promptly when a busy org floods the queue. |

## Adding a rule or policy

Implement `ScoreRule` for a new contribution, add it to a rule list in
`policy.rs` (or a new list), and register the named policy in `policy_by_name`.
Each rule has unit tests in `backend/score/src/rules`; run them with
`cargo test -p score`.
