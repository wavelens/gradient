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
| `EstimatedTimeRule` | Bonus, up to 3600 | Builds expected to finish soonest on this worker, see [Estimated Time](#estimated-time) |
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
| `ResourceSaturationRule` | Penalty, up to -17200 | Worker above 80% CPU (90% for `builtin` jobs) or below 10% free memory, or a likely out-of-memory build |
| `PreferLocalBuildRule` | Bonus | `preferLocalBuild` derivations on a worker holding most of the closure |
| `FairShareRule` | Penalty, disabled | Would slow projects holding a large share of running work |

## Estimated Time

`EstimatedTimeRule` can sum the seconds a build should take on the worker. Each second costs one point below the cap of 3600. Workers already holding every output get the full 3600.

| Part | Estimate |
|---|---|
| Download | Missing NAR size over the slower of the worker's download speed and its share of the storage read throughput |
| Build | Build time of earlier builds of the package, scaled by their CPU score over the worker's, plus 4% per build already running there |
| Upload | Output size of earlier builds over the slower of the worker's upload speed and its share of the storage write throughput |

- The storage share is the best total throughput of the last hour, split across the transfers in flight plus this one.
- The storage throughput will be 150 MB/s for reads and 140 MB/s for writes before the first measurement.
- A package without history can use the instance's mean build time.
- The CPU score ratio must stay between 0.5 and 2.
- Workers measure upload, download and disk speed from their own NAR transfers, substitutions and builds. Transfers under 1 MiB stay out.

The cap of 3600 lies below the 4000 of `WaitTimeRule`. A long-waiting job can always overtake a shorter one. A prioritized job with its 5000 can outrank any estimate gap. The penalty of `ResourceSaturationRule` includes the cap and can still keep a build off a saturated worker.

## Custom Policies

A policy is a named list of rules. Every rule is a small, separately tested scoring function. Production systems with special placement needs (license-bound machines, a fixed build order, cost-based routing) can get a custom policy without touching the scheduler.

1. Write a rule as one function from the job, the worker and the averages of the instance to a score. The rule also needs a description for the Job Board.
2. Combine the rule and the existing ones into a new named policy (in `backend/gradient-pool/src/score/policy.rs`).
3. Add the name to the allowed values of `scheduler.scoringPolicy` (in `nix/modules/gradient.nix`). Select the policy there.

The Job Board will then show the new rule's share in every assignment decision, next to the built-in rules.

## Related

- [Workers](../concepts/workers.md#matching-builds): which workers a job can go to at all
- [Scoring internals](../contributors/scheduler/scoring.md): contexts, windows and adding a rule
