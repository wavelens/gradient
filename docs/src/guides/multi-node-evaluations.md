# Run Multi-Node Evaluations

Up to four workers share one evaluation of a large flake. New commits wait less.

**Requirements:**

- At least two workers that carry the `eval` capability. Setup steps live in [Add a Remote Worker](remote-worker.md). Shared workers (`worker.capabilities.federate`) never take part
- Enough free RAM on every worker to hold the largest attribute set of the task
- A completed evaluation of the task. Cost estimates come from it

## 1. Enable the Task

=== "UI"

    Open the task settings and check **Enable MultiNode Evaluations (beta)**.

=== "Declarative"

    ```nix
    services.gradient.state.tasks.nixpkgs-fork.multi_node_evaluation = true;
    ```

## 2. Check the Split Rules

Server-side decisions happen once per evaluation, before routing. Splits require every condition below.

| Condition | Rule |
|---|---|
| History | The task has a finished evaluation. The first evaluation stays on one worker |
| Idle workers | At least 2 idle eval workers. At most 4 workers share one evaluation |
| Gain | The predicted wall clock with the split stays at or below 90 % of one worker alone |
| Free RAM | Every worker needs free RAM for the largest set peak of the task |
| Placement | Too few free workers shrink the split. Below 2 workers, or after 30 s without placement, the evaluation runs on one worker |

## 3. Shape the Split Points

A [wildcard](../reference/wildcards.md) in the task can mark where evaluation splits. Children of the first `*` form candidate units of work.

```
legacyPackages.x86_64-linux.**
```

- `*` can split one level, for example `packages.x86_64-linux.*`.
- `**` can also turn nested sets discovered during listing into units of their own. Large sets like `python3Packages` then split further.

## Verify Deployment

- **Job Board -> Live Jobs** can show one eval row per worker for the evaluation. One of those workers, the primary, fetched the flake. The others are companions.
- The thunk counter of the evaluation shows the sum across all workers.
- Lost workers give back only the running unit of work. The other workers continue.
- The server log line `split cluster dissolved; its evaluation runs on one worker` means no free seats within 30 s.

## Beta Caveats

- Split decisions happen once per evaluation and server start. A restart can split an evaluation again.
- Split evaluations never wait for workers. Busy fleets run the evaluation on one worker.
- Nested sets found during listing carry no cost estimate.
- An attempt with every worker lost retries up to 2 times.

## Next Steps

- [Set Up Clusters](clusters.md)
- [Tune Evaluation Speed](tune-evaluations.md)
- [Evaluation Wildcards](../reference/wildcards.md)
- [Cluster Jobs: Split Evaluations](../contributors/scheduler/clusters.md#split-evaluations)
