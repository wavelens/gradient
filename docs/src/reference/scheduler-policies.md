# Scheduler Policies

Each time a worker asks for work, the scheduler scores every queued job that worker can run. The job with the highest score wins. A score is the sum of all rules in the active policy. The [Job Board](../ui/job-board.md#job-inspection) breaks down each rule's contribution for every assigned job.

```nix
services.gradient.scheduler.scoringPolicy = "resource-aware"; # (1)!
```

1.  Choose `resource-aware` or `simple`, see the table below.

| Policy | Rules | Pick when |
|---|---|---|
| `resource-aware` (default) | All rules below | Workers report metrics and heavy builds need to land on machines that fit them |
| `simple` | The first table only | Workers lack metrics, or placement by estimated time and wait time alone is enough |

Unknown policy names fall back to `resource-aware`.

## Negative Scores

Workers take the job with the highest total score. Only jobs with a total of at least 0 qualify, and no rule can veto them. A vetoed or negative job waits for a better fit. Workers idle for a round when no job qualifies. Penalties and vetoes are the only way to hold a job back.

## Rules in Both Policies

| Rule | Kind | Effect |
|---|---|---|
| `EstimatedTimeRule` | Bonus, up to 3600 | Favor jobs that this worker can likely finish soonest, see [Estimated Time](#estimated-time) |
| `WaitTimeRule` | Bonus, up to 4000 | Raise the score of jobs that wait long, to prevent starvation. The wait count start at the moment all dependencies finish |
| `QosRule` | Bonus, 5000, 1000 and 1000 | Put prioritized jobs above all others. Jobs of a build request ([`gradient build`](../guides/build-before-push.md) or [SSH](../guides/build-over-ssh.md)) receive another 1000. Builds of an imported derivation and their unfinished dependencies also receive 1000 while evaluations wait on the import |
| `RescoreWaitRule` | Veto | Hold a build until a worker report the size of its missing data. The hold lift after 4 rounds |
| `ReserveFetchWorkersRule` | Penalty, up to 300 | Keep fetch-capable workers free for fetching while capacity run short |
| `TransferLimitRule` | Penalty, 4000 | Hold large transfers while all transfer slots sit full, see [Transfer Limits](#transfer-limits) |

The **Prioritize** entry in the task or evaluation menu set the `QosRule` flag on a build and its dependencies, or on a whole evaluation. A failed or aborted build or evaluation clear the flag. Only superusers can prioritize.

## Rules in `resource-aware` Only

The saturation check can use live worker load and the predicted peak memory. The prediction needs earlier builds of the same package.

| Rule | Kind | Effect |
|---|---|---|
| `ResourceSaturationRule` | Penalty, up to -17200, and veto | Worker above 80% CPU (90% for `builtin` jobs) or below 10% free memory, or a build above the free memory |
| `FairShareRule` | Penalty, disabled | Would slow projects holding a large share of running work |

- To compute the free memory of a worker, take the measured value and subtract the memory reserved by jobs already assigned to that worker.
- Every assigned job can reserve its predicted peak memory for the whole duration of the job.
- Workers with `worker.build.cgroup` report the peak memory of each running build every 10 seconds. When a build go above its prediction, it will reserve the reported peak instead.
- On such a worker, reserved memory that no build has touched yet also count against the measured free memory.
- If a build get lost together with its worker, the last reported peak remain in the history as a sample.
- A build without history can reserve the mean peak memory of the instance's builds over the last 24 hours.
- Substitutions, fetches and evaluations make no reservations.
- A build that demand more than the free memory get a veto and wait until the memory reserved by the assigned jobs on that worker become available.
- A build that demand more than the worker's total memory, or that come up short on a worker with no reservations, take the penalty alone.
- A build held for 60 seconds, or a prioritized build, can keep its memory free on a single worker.
- Other builds have to fit beside that memory. As soon as the held build fit, it will start on that worker.

## Estimated Time

`EstimatedTimeRule` can estimate how many seconds a job will take on a worker. Each predicted second cost one point, up to a cap of 3600.

| Part | Estimate |
|---|---|
| Download | Missing NAR size divided by the slower of the worker's download speed and its share of the storage read throughput |
| Paths | Missing paths times the per-path time of recent prefetches |
| Build | Build time of earlier builds of the package, scaled by their CPU score over the worker's, plus 4% per build already running there |
| Out of memory | Build time again, weighted by the chance of an out-of-memory kill |
| Upload | Output size divided by the slower of the worker's upload speed and its share of the storage write throughput |

- A worker that already has every output only performs the upload.
- All outputs in an upstream cache remove the build. The download then amounts to a time per output plus a time per megabyte.
- A least-squares fit over the substitutions of the last 24 hours can yield both of those times. With fewer than 20 substitutions, the download speed stand in as the default.
- The job's own outputs can supply the output size once all of them have a size. An upstream narinfo can supply it before any build, and otherwise earlier builds of the package can.
- An evaluation can take the mean fetch time and mean evaluation time of its task over the last 7 days. A job only counts the parts it actually performs, so downloads and paths drop out.
- With no recorded runs of a part for a task, the mean across all tasks fill in.
- The out-of-memory probability is the package's kill rate plus the fraction of predicted peak memory above free memory, capped at 1.
- The storage share split the best total throughput of the last hour across the transfers in flight plus this one.
- Storage throughput default to 150 MB/s for reads and 140 MB/s for writes until the first measurement.
- A least-squares fit of prefetch time over its megabytes and paths in the last 24 hours can yield the per-path time. With fewer than 100 prefetches, each path cost 0.18 s.
- A package without build history can use the instance's median build time. Most builds finish in under a second, and a few long builds pull the mean far above a typical build.
- Clamp the CPU score ratio between 0.5 and 2.
- Workers measure upload, download and disk speed from their own NAR transfers, substitutions and builds. Transfers under 1 MiB drop out of these measurements.

The cap of 3600 lie below the 4000 of `WaitTimeRule`, so a long-waiting job can always overtake a shorter one. A prioritized job, with its 5000, can outrank any estimate gap. The penalty of `ResourceSaturationRule` include the cap and can still keep a build off a saturated worker.

## Transfer Limits

`TransferLimitRule` holds a build below 0 while all server slots stay busy.

| Slots | Full at | Held build |
|---|---|---|
| Downloads | Builds in prefetch reach [`nar.maxConcurrentDownloads`](configuration.md#nar) | Missing NAR size above the mean of the last hour |
| Uploads | Builds in upload reach [`upload.concurrency`](configuration.md#upload) | Expected output above 1 MiB |

- A build with local inputs or a small output can still take a free worker slot.
- The hold of 4000 match the cap of `WaitTimeRule`. Once the wait bonus make up for the hold, the build reach 0.

## Custom Policies

A policy is a named set of rules. Each rule is a small scoring function with its own tests. Production setups with special placement needs, such as license-bound machines, a fixed build order or cost-based routing, can add a policy without changes to the scheduler.

1. Write one rule as a single function. The input consists of the job, the worker and the instance averages; the output is a score. Add a description so the Job Board can display it.
2. Combine the rule with the existing ones into a new named policy in `backend/gradient-pool/src/score/policy.rs`.
3. Add the name to the allowed values of `scheduler.scoringPolicy` in `nix/modules/gradient.nix` and select the policy there.

The Job Board then shows the new rule's share in every assignment decision, next to the built-in rules.

## Related

- [Workers](../concepts/workers.md#matching-builds): the rules that decide which workers can take a job
- [Scoring internals](../contributors/scheduler/scoring.md): how contexts and windows work, plus how to add a rule
