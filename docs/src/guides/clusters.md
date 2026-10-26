# Set Up Clusters

Workers carry a datacenter label and an address for cluster peers.

Multiple jobs form one cluster job. They share a start time and run on separate workers. All run or none run. The only cluster jobs today are [multi-node evaluations](multi-node-evaluations.md). Internals appear on the [Cluster Jobs](../contributors/scheduler/clusters.md) page.

**Requirements:**

- The datacenter of every worker host
- Two workers at minimum, with setup steps in [Add a Remote Worker](remote-worker.md)

## 1. Label the Zones

Set `worker.zone` to the datacenter of a worker. Workers with the same label share one zone. Workers without a label share one unnamed zone.

```nix
# configuration.nix of every worker in Frankfurt
services.gradient.worker.zone = "fra1";
```

- Cluster jobs with one zone place every member in that zone.
- Multi-node evaluations take members from any zone. Later cluster jobs still use the labels.

## 2. Advertise the Endpoints

Other members reach a worker at `worker.endpoint`. All members receive the address from the server unchanged at cluster start.

```nix
# configuration.nix of a worker
services.gradient.worker.endpoint = "10.0.0.7:7000"; # (1)!
```

1.  Set the option to `null` for no advertised address. Members of multi-node evaluations route everything through the server. They need no endpoint.

## 3. Tune the Scheduler

Cluster jobs wait for a duration that three server options set.

| Option | Default | Change when |
|---|---|---|
| `scheduler.clusterPrepareTimeoutSecs` | `30` | Slow worker links make members miss the acceptance window |
| `scheduler.clusterReserveAfterSecs` | `600` | Busy fleets rarely have enough idle workers at once |
| `scheduler.clusterReserveTimeoutSecs` | `1800` | Reserved workers sit idle too long before the cluster starts |

```nix
# configuration.nix of the server
services.gradient.scheduler.clusterPrepareTimeoutSecs = 60;
```

- Members have `clusterPrepareTimeoutSecs` to accept the assignment. Missed deadlines end the attempt. Cluster jobs then wait again.
- Cluster jobs reserve workers after a wait of `clusterReserveAfterSecs`. No new single jobs go to reserved workers.
- Multi-node evaluations never reserve workers. Multi-node evaluations without free seats after 30 s run on one worker.

## Verify Deployment

- The rows under **Job Board -> Live Jobs** show one job per member, one row per worker, and all of them point to a single evaluation.
- Check the server log for `evaluation split into a cluster job` when a split starts.
- Read `split cluster dissolved; its evaluation runs on one worker` in the log as a sign that no free seats appeared within 30 s.

Waiting cluster jobs take no worker. Single jobs continue to run until the cluster can start.

## Next Steps

- [Run Multi-Node Evaluations](multi-node-evaluations.md)
- [Workers: Zones](../concepts/workers.md#zones)
- [Configuration Reference](../reference/configuration.md)
- [Cluster Jobs](../contributors/scheduler/clusters.md)
