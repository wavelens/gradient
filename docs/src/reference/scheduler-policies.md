# Scheduler Policies

When a worker asks for work, the scheduler scores every queued job the worker can take and hands out the highest. The score is the sum of the policy's rules; the [Job Board](../ui/job-board.md#job-inspection) shows each rule's share for every dispatched job.

```nix
services.gradient.scheduler.scoringPolicy = "resource-aware"; # (1)!
```

1.  `resource-aware` or `simple`, see the table below.

| Policy | Rules | Pick when |
|---|---|---|
| `resource-aware` (default) | All rules below | Workers report metrics; heavy builds should land on machines that fit them |
| `simple` | The first table only | Workers without metrics, or placement by cache warmth and wait time alone |

An unknown name falls back to `resource-aware`.

## Negative Scores

The worker's highest-scoring job is handed out only when its total is at least 0 and no rule vetoes the job. Otherwise the worker idles this round and the job waits for a better fit. Bonus rules never go below zero; only penalties and vetoes can hold a job back.

## Rules in Both Policies

| Rule | Kind | Effect |
|---|---|---|
| `MissingPathsRule` | Bonus, up to 200 | Worker already holds most of the inputs |
| `MissingNarSizeRule` | Bonus, up to 500 | Little data to download before the build |
| `RealisedOutputsRule` | Bonus, 2500 | Worker already holds every output and only uploads them |
| `DependencyCountRule` | Bonus, up to 50 | Builds with many direct inputs |
| `WaitTimeRule` | Bonus, growing | Long-waiting jobs rise, against starvation; counted from the moment dependencies finished |
| `BuiltinDeprioritizeRule` | Bonus, 50 or 100 | Real builds before `builtin` downloads; `builtin` jobs still reach workers without systems |
| `QosRule` | Bonus, 5000 | Prioritized jobs beat every other job |
| `RescoreWaitRule` | Veto | Holds a build until a worker reported its missing data size; lifted after 4 rounds |
| `ReserveFetchWorkersRule` | Penalty | Keeps fetch-capable workers free for fetching while capacity is short |

**Prioritize** in the task or evaluation menu sets the `QosRule` flag on a build and its dependencies, or on a whole evaluation. The flag clears when the build or evaluation fails or is aborted.

## Rules in `resource-aware` Only

These rules need `services.gradient.worker.build.metrics` on the workers and earlier runs of the same package; without either, they add nothing.

| Rule | Kind | Effect |
|---|---|---|
| `ResourceFitRule` | Penalty | Predicted peak memory above the worker's free memory; builds and evaluations |
| `ResourceSaturationRule` | Penalty, up to -10000 | Worker above 80% CPU (90% for `builtin` jobs) or below 10% free memory, or a likely out-of-memory build |
| `PreferLocalBuildRule` | Bonus | `preferLocalBuild` derivations on a worker holding most of the closure |
| `NetworkAffinityRule` | Bonus | Fixed-output downloads on workers with fast network |
| `DiskAffinityRule` | Bonus | Disk-heavy builds on workers with fast disks |
| `CpuAffinityRule` | Bonus or penalty, up to 1200 | Long builds on faster cores than the fleet average, away from slower ones |
| `FairShareRule` | Penalty, disabled | Would slow projects holding a large share of running work |

Workers measure network and disk speed from their own NAR transfers and builds; until the first transfer, the affinity rules add nothing.

## Custom Policies

A policy is a named list of rules, and every rule is a small, separately tested scoring function. Production systems with special placement needs, e.g. license-bound machines, a fixed build order or cost-based routing, get a custom policy without touching the scheduler:

1. Write a rule: one function from the job, the worker and the instance averages to a score, plus a description for the Job Board.
2. Combine the rule with the existing ones into a new named policy in `backend/gradient-pool/src/score/policy.rs`.
3. Add the name to the allowed values of `scheduler.scoringPolicy` in `nix/modules/gradient.nix` and select the policy there.

The Job Board then shows the new rule's share in every dispatch decision, next to the built-in rules.

## Related

- [Workers](../concepts/workers.md#matching-builds): which workers a job can go to at all
- [Scoring internals](../contributors/scheduler/scoring.md): contexts, windows and adding a rule
