<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Scheduler Policies

The scheduler will score every queued job a worker can take on each request for work. The highest-scoring job will go to the worker. The score is the sum of the policy's rules. The [Job Board](../ui/job-board.md#job-inspection) can show each rule's share for every assigned job.

```nix
services.gradient.scheduler.scoringPolicy = "resource-aware"; # (1)!
```

1.  `resource-aware` or `simple`, see the table below.

| Policy | Rules | Pick when |
|---|---|---|
| `resource-aware` (default) | All rules below | Workers reporting metrics, with heavy builds meant for machines that fit them |
| `simple` | The first table only | Workers without metrics, or placement by estimated time and wait time alone |

An unknown name will fall back to `resource-aware`.

## Negative Scores

The worker will get its highest-scoring job with a total of at least 0 and no veto from any rule. A vetoed or negative job will wait for a better fit. The worker will idle this round without any eligible job. Only penalties and vetoes can hold a job back.

## Rules in Both Policies

| Rule | Kind | Effect |
|---|---|---|
| `EstimatedTimeRule` | Bonus, up to 3600 | Jobs expected to finish soonest on this worker, see [Estimated Time](#estimated-time) |
| `WaitTimeRule` | Bonus, up to 4000 | Long-waiting jobs rising, against starvation. Counted from the moment dependencies finished |
| `QosRule` | Bonus, 5000, 1000 and 1000 | Prioritized jobs beating every other job. Jobs of a build request ([`gradient build`](../guides/build-before-push.md) or [SSH](../guides/build-over-ssh.md)) gaining another 1000. Builds of an imported derivation and their unfinished dependencies gaining 1000 as well while evaluations wait on the import |
| `RescoreWaitRule` | Veto | Holding a build until a worker reported its missing data size. Lifted after 4 rounds |
| `ReserveFetchWorkersRule` | Penalty, up to 300 | Keeping fetch-capable workers free for fetching while capacity is short |
| `TransferLimitRule` | Penalty, 4000 | Holding large transfers while the transfer slots are full, see [Transfer Limits](#transfer-limits) |

The **Prioritize** entry in the task or evaluation menu can set the `QosRule` flag on a build and its dependencies, or on a whole evaluation. The flag will clear on a failed or aborted build or evaluation.

## Rules in `resource-aware` Only

The saturation check can use live worker load and the predicted peak memory. The prediction needs earlier builds of the same package.

| Rule | Kind | Effect |
|---|---|---|
| `ResourceSaturationRule` | Penalty, up to -17200 | Worker above 80% CPU (90% for `builtin` jobs) or below 10% free memory, or a likely out-of-memory build |
| `FairShareRule` | Penalty, disabled | Would slow projects holding a large share of running work |

## Estimated Time

`EstimatedTimeRule` can sum the seconds a job should take on the worker. Each second costs one point below the cap of 3600.

| Part | Estimate |
|---|---|
| Download | Missing NAR size over the slower of the worker's download speed and its share of the storage read throughput |
| Paths | Missing paths times the per-path time of recent prefetches |
| Build | Build time of earlier builds of the package, scaled by their CPU score over the worker's, plus 4% per build already running there |
| Out of memory | Build time again, weighted by the chance of an out-of-memory kill |
| Upload | Output size over the slower of the worker's upload speed and its share of the storage write throughput |

- A worker already holding every output only needs the upload.
- A job with all outputs in an upstream cache can skip the build. Its download is a time per output plus a time per megabyte.
- A least-squares fit of the substitutions in the last 24 hours can yield both times. The download speed will stand in before 20 substitutions exist.
- The job's own outputs can give the output size once all of them have a size. An upstream narinfo can fill the size before any build. Earlier builds of the package can give the size otherwise.
- An evaluation can take the mean fetch time and the mean evaluation time of its task over the last 7 days. A job adds only the parts it runs. Downloads and paths stay out.
- A task without runs of a part can use the mean of all tasks.
- The chance of an out-of-memory kill is the package's kill rate plus the share of predicted peak memory above free memory. The chance can reach at most 1.
- The storage share is the best total throughput of the last hour, split across the transfers in flight plus this one.
- The storage throughput will be 150 MB/s for reads and 140 MB/s for writes before the first measurement.
- A least-squares fit of the prefetch time over its megabytes and paths in the last 24 hours can yield the per-path time. Every path can cost 0.18 s before 100 prefetches exist.
- A package without history can use the instance's median build time. Most builds take under a second. A few long builds pull the mean far above a typical build.
- The CPU score ratio must stay between 0.5 and 2.
- Workers measure upload, download and disk speed from their own NAR transfers, substitutions and builds. Transfers under 1 MiB stay out.

The cap of 3600 lies below the 4000 of `WaitTimeRule`. A long-waiting job can always overtake a shorter one. A prioritized job with its 5000 can outrank any estimate gap. The penalty of `ResourceSaturationRule` includes the cap and can still keep a build off a saturated worker.

## Transfer Limits

`TransferLimitRule` can hold a build below 0 while the slots of the server are full.

| Slots | Full at | Held build |
|---|---|---|
| Downloads | Builds in prefetch reaching [`nar.maxConcurrentDownloads`](configuration.md#nar) | Missing NAR size above the mean of the last hour |
| Uploads | Builds in upload reaching [`upload.concurrency`](configuration.md#upload) | Expected output above 1 MiB |

- A build with local inputs or a small output can take the free worker slot instead.
- The hold of 4000 is as large as the cap of `WaitTimeRule`. A held build will reach 0 once its wait bonus makes up for the hold.

## Custom Policies

A policy is a named list of rules. Every rule is a small, separately tested scoring function. Production systems with special placement needs (license-bound machines, a fixed build order, cost-based routing) can get a custom policy without touching the scheduler.

1. Write a rule as one function from the job, the worker and the averages of the instance to a score. The rule also needs a description for the Job Board.
2. Combine the rule and the existing ones into a new named policy (in `backend/gradient-pool/src/score/policy.rs`).
3. Add the name to the allowed values of `scheduler.scoringPolicy` (in `nix/modules/gradient.nix`). Select the policy there.

The Job Board will then show the new rule's share in every assignment decision, next to the built-in rules.

## Related

- [Workers](../concepts/workers.md#matching-builds): which workers a job can go to at all
- [Scoring internals](../contributors/scheduler/scoring.md): contexts, windows and adding a rule
