# Scheduler Policies

The scheduler will score every queued job a worker can take on each request for work. The highest-scoring job will go to the worker. The score is the sum of the policy's rules. The [Job Board](../ui/job-board.md#job-inspection) can show each rule's share for every assigned job.

```nix
services.gradient.scheduler.scoringPolicy = "resource-aware"; # (1)!
```

1.  `resource-aware` or `simple`, see the table below.

| Policy | Rules | Pick when |
|---|---|---|
| `resource-aware` (default) | All rules below | Workers reporting metrics, with heavy builds meant for machines that fit them |
| `simple` | The first table only | Workers without metrics, or placement by cache warmth and wait time alone |

An unknown name will fall back to `resource-aware`.

## Negative Scores

The worker will get its highest-scoring job with a total of at least 0 and no veto from any rule. A vetoed or negative job will wait for a better fit. The worker will idle this round without any eligible job. Bonus rules are never going below zero. Only penalties and vetoes can hold a job back.

## Rules in Both Policies

| Rule | Kind | Effect |
|---|---|---|
| `MissingPathsRule` | Bonus, up to 200 | Worker already holding most of the inputs |
| `MissingNarSizeRule` | Bonus, up to 500 | Little data to download before the build |
| `RealisedOutputsRule` | Bonus, 2500 | Worker already holding every output and only uploading them |
| `DependencyCountRule` | Bonus, up to 50 | Builds with many direct inputs |
| `WaitTimeRule` | Bonus, growing | Long-waiting jobs rising, against starvation. Counted from the moment dependencies finished |
| `BuiltinDeprioritizeRule` | Bonus, 50 or 100 | Real builds before `builtin` downloads. `builtin` jobs still reaching workers without systems |
| `QosRule` | Bonus, 5000 and 1000 | Prioritized jobs beating every other job. Jobs of a build request ([`gradient build`](../guides/build-before-push.md) or [SSH](../guides/build-over-ssh.md)) gaining another 1000 |
| `RescoreWaitRule` | Veto | Holding a build until a worker reported its missing data size. Lifted after 4 rounds |
| `ReserveFetchWorkersRule` | Penalty | Keeping fetch-capable workers free for fetching while capacity is short |

The **Prioritize** entry in the task or evaluation menu can set the `QosRule` flag on a build and its dependencies, or on a whole evaluation. The flag will clear on a failed or aborted build or evaluation.

## Rules in `resource-aware` Only

The memory predictions (`ResourceFitRule`, the out-of-memory check) are requiring `services.gradient.worker.build.metrics` on the workers. They also need earlier builds of the same package. The CPU and memory saturation check will use live worker load.

| Rule | Kind | Effect |
|---|---|---|
| `ResourceFitRule` | Penalty | Predicted peak memory above the worker's free memory, for builds and evaluations |
| `ResourceSaturationRule` | Penalty, up to -10000 | Worker above 80% CPU (90% for `builtin` jobs) or below 10% free memory, or a likely out-of-memory build |
| `PreferLocalBuildRule` | Bonus | `preferLocalBuild` derivations on a worker holding most of the closure |
| `NetworkAffinityRule` | Bonus | Fixed-output downloads on workers with fast network |
| `DiskAffinityRule` | Bonus | Disk-heavy builds on workers with fast disks |
| `CpuAffinityRule` | Bonus or penalty, up to 1200 | Long builds on faster cores than the fleet average, away from slower ones |
| `FairShareRule` | Penalty, disabled | Would slow projects holding a large share of running work |

Workers are measuring network and disk speed from their own NAR transfers and builds. The affinity rules are adding nothing until the first transfer.

## Custom Policies

A policy is a named list of rules. Every rule is a small, separately tested scoring function. Production systems with special placement needs (license-bound machines, a fixed build order, cost-based routing) can get a custom policy without touching the scheduler.

1. Write a rule as one function from the job, the worker and the averages of the instance to a score. The rule also needs a description for the Job Board.
2. Combine the rule and the existing ones into a new named policy (in `backend/gradient-pool/src/score/policy.rs`).
3. Add the name to the allowed values of `scheduler.scoringPolicy` (in `nix/modules/gradient.nix`). Select the policy there.

The Job Board will then show the new rule's share in every assignment decision, next to the built-in rules.

## Related

- [Workers](../concepts/workers.md#matching-builds): which workers a job can go to at all
- [Scoring internals](../contributors/scheduler/scoring.md): contexts, windows and adding a rule
