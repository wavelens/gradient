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

- `ScoringPolicy` (`policy.rs`): `name()` plus
  `score(&JobContext, &WorkerContext, &InstanceContext) -> f64`.
- `RulePolicy`: a named `Vec<Box<dyn ScoreRule>>` whose `score` sums each rule.
- `ScoreRule` (`rule.rs`): one
  `score(&JobContext, &WorkerContext, &InstanceContext) -> f64` contribution.
- Three scoring inputs:
  - `JobContext` — per-candidate: kind, architecture, missing-path count,
    missing NAR size, dependency count, `queued_at`/`ready_at`, `rescore_count`
    and the owning org's work share.
  - `WorkerContext` — the worker's architectures, system features, fetch
    capability and optional live metrics (free RAM, CPU-core score, disk/network
    speed).
  - `InstanceContext` — instance-wide 5m/1h/24h windowed averages plus live
    counts (active/pending builds, total/idle workers), recomputed every
    `GRADIENT_INSTANCE_METRICS_INTERVAL` seconds (default 30) and used to make
    soft-rule thresholds instance-relative.
- `ScoredJob` exposes lazy providers (`closure_size`, `history`) so a policy
  pays for closure/history lookups only when a rule reads them.

The scheduler builds the contexts, calls the configured policy per candidate,
and assigns the worker its top-scoring job — unless that score is negative.

## Soft rules vs. disqualifiers

Each rule is one of two classes:

- **Soft rule** — returns a bounded `[0, cap]` bonus, with the threshold for
  saturation taken from the instance window (e.g. `MissingNarSizeRule` caps at
  500, `MissingPathsRule` at 200, `DependencyCountRule` at 50, `WaitTimeRule`
  scaled by the instance average wait). Soft rules never push a job below zero.
- **Disqualifier** — may go negative (`RescoreWaitRule`, `FairShareRule`,
  `BuiltinDeprioritizeRule`, `ReserveFetchWorkersRule`). `ResourceFitRule` is
  mixed: its RAM-overshoot side is a disqualifier; its CPU-affinity side is a
  soft bonus.

`take_best_of_kind` will not dispatch a candidate whose **total** score is
negative — the worker idles that round and the job is retried next cycle. This
gates builds the worker cannot yet serve well rather than forcing a bad
placement.

### `RescoreWaitRule`

A build for which no worker has yet reported `missing_nar_size` scores `-1000`,
holding it back until the cache state is known. After `rescore_count` reaches 4
dispatch rounds the penalty drops to 0 so the build stops blocking. Eval jobs
are never penalized.

### `WaitTimeRule`

Wait is measured from `ready_at` (when the job's dependencies cleared), not
`queued_at`, so dependency wait is excluded. The bonus grows with wait, scaled
by the instance average wait, and saturates at the rule cap for anti-starvation.

### `FairShareRule`

Penalty proportional to the owning org's share of in-flight **work** —
weighted by build duration (prefer-local builds count at half), not by job
count — so a few long builds and many short ones are balanced fairly.

## `simple` policy rules

| Rule | Class | Effect |
|---|---|---|
| `MissingPathsRule` | soft | `[0,200]` bonus for path availability the worker can serve, instance-relative to the average missing-path count. |
| `MissingNarSizeRule` | soft | `[0,500]` bonus for low fetch size, scaled by the instance average NAR size. |
| `DependencyCountRule` | soft | `[0,50]` bonus per dependency for build jobs (unblocks more downstream work first). |
| `WaitTimeRule` | soft | Bonus growing with `ready_at` wait, scaled by the instance average wait, for anti-starvation. |
| `RescoreWaitRule` | disqualifier | `-1000` for a build with no reported `missing_nar_size`, until `rescore_count` hits 4; never penalizes eval. |
| `BuiltinDeprioritizeRule` | disqualifier | Penalty for `builtin`-architecture build jobs. |
| `ReserveFetchWorkersRule` | disqualifier | Penalty when a fetch-capable worker is offered a cached-eval job, relaxed as idle capacity grows. |

## `resource-aware` policy rules

Adds the following on top of the `simple` rule set:

| Rule | Class | Effect |
|---|---|---|
| `ResourceFitRule` | soft + disqualifier | Penalty scaling with predicted-RAM overshoot of free RAM (amplified by past/instance OOM rate); bonus for CPU-heavy jobs on higher-CPU-score workers. Now also applies to **evaluation** jobs (previously builds-only), using a per-project p95 of historical eval peak-RSS so heavy evals route to big-RAM workers. No-op without history samples or worker metrics. |
| `PreferLocalBuildRule` | soft | Bonus for `preferLocalBuild` derivations on a worker that already holds (most of) the closure, decaying with missing paths. |
| `FairShareRule` | disqualifier | Penalty proportional to the org's share of in-flight work (duration-weighted; prefer-local at half), so a quiet org is served promptly when a busy org floods the queue. |
| `NetworkAffinityRule` | soft | Bonus for fixed-output derivations on faster-network workers, scaling to a reference speed then capping. No-op for non-FOD jobs or without a network metric. |
| `DiskAffinityRule` | soft | Bonus for disk-heavy jobs on faster-disk workers, scaling to a reference speed then capping. No-op below the disk-heavy threshold or without a disk metric. |

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
