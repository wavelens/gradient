# Workers

A **worker** is a machine running `gradient-worker`. The worker is connecting to the server and taking jobs queued by projects with the worker enabled. The worker is sending the results back. The server itself is running no Nix. Every clone, evaluation and build is taking place on a worker.

```mermaid
flowchart LR
    fetch[Fetch] --> eval[Evaluate] --> build[Build]
```

## Capabilities

Each worker is advertising which of the three job kinds the worker can take.

| Capability | Job |
|---|---|
| Fetch | Clone the repository and download the flake inputs |
| Evaluate | Walk the flake and report derivations |
| Build | Build derivations and upload the outputs |

One machine can take all three, or the kinds can be split, e.g. a large-memory machine for evaluation and several smaller builders.

## Matching Builds

A build is only going to a worker supporting the derivation's system (e.g. `aarch64-linux`) and every required system feature (e.g. `kvm`, `big-parallel`). Systems come from `worker.system.architectures`, with the host platform as default. The worker is detecting features from the local Nix. `worker.system.features` can override the detected features.

The scheduler is scoring each queued job among the matching workers. The scheduler is steering heavy builds and evaluations away from workers lacking free memory for the predicted peak. Earlier builds are the source of this prediction. `services.gradient.worker.build.metrics` is recording the per-build measurements behind this prediction.

## Zones

Workers in one datacenter share a zone label, `services.gradient.worker.zone` (default: none). A [cluster job](../contributors/scheduler/clusters.md) needing a fast interconnect is starting all members in one zone. All workers without a zone share one unnamed zone.

`services.gradient.worker.endpoint` is the address the other members of a cluster reach the worker at. The server is handing every member the endpoints of the others at cluster start.

## Access

| Kind | Registered | Serving |
|---|---|---|
| Project worker | Under one project | That project |
| Team worker | Under one [team](teams.md) | Every project granting the team's workers |
| Local worker | On the server host, automatically | Every new project, as a worker of the state-declared team `server` |

A worker is only receiving jobs from projects with a cache subscription.

## Team Workers

A team worker is part of one [team](teams.md), with one token for the whole team. Granted projects list the team worker read-only. Changes take place on the team's **Workers** page.

| Setting | Effect |
|---|---|
| Grant with workers | Team worker active in the project |
| `new_projects.workers` | Team workers granted to every new project |
| `enabled` | Global switch for the worker in every granted project |

A worker ID is either a team worker or a set of project registrations, never both.

## Ephemeral Workers

A worker in a throwaway VM can announce draining. The server is then no longer sending new jobs. The running jobs finish. A fresh VM can then replace the old one.

## Related

- [Quick Start](../get-started/quick-start.md): the local worker
- [Add a Remote Worker](../guides/remote-worker.md): add build machines
- [Evaluations and Builds](evaluations-and-builds.md): what workers run
