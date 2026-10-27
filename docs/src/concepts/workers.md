# Workers

A **worker** is a machine with `gradient-worker` installed. Workers connect to the server, take jobs from the queues of projects with the worker feature enabled, and return the results to the server. The server holds no Nix. A worker handles every clone, evaluation and build.

```mermaid
flowchart LR
    fetch[Fetch] --> eval[Evaluate] --> build[Build]
```

## Capabilities

Workers declare their capabilities at the handshake. Capabilities cover the jobs and the access of a worker.

| Capability | Default | Meaning |
|---|---|---|
| `network` | on | Internet access. Needed to clone the repository, download missing inputs, substitute from upstream caches and run fixed-output builds |
| `secrets` | on | The server may send the project SSH key. Needed to clone a repository behind an SSH URL |
| `exec` | off | Start [exec jobs](../guides/exec-jobs.md) on the worker |
| `eval` | on | Walk the flake and report derivations |
| `build` | on | Build derivations and upload the outputs |

- A worker with `network` can perform the fetch step for public repositories.
- A worker with `network` and `build` can run fixed-output builds.
- A worker with `network` alone can take substitutes and downloads. No Nix build is involved in either.
- Workers without `secrets` never get an SSH key.
- An evaluating worker never downloads an input itself. The server can hand each missing input to a worker with `network`, plus `secrets` for an SSH URL. The evaluation can then pull the input from the cache.
- A project or team registration can withhold each capability through `enable_network`, `enable_secrets`, `enable_exec`, `enable_eval` and `enable_build`.

A single machine can hold every capability. A machine with a lot of memory can evaluate while smaller machines build.

## Matching Builds

Builds only go to workers supporting the derivation's system (e.g. `aarch64-linux`) and every required system feature (e.g. `kvm`, `big-parallel`). Systems come from `worker.system.architectures`, with the host platform as default. Workers read features from their local Nix store. Experimental features such as `recursive-nix` add a system feature to that store once enabled. `worker.system.features` can override the detected features.

The scheduler will score each queued job among the matching workers. The scheduler will also keep heavy builds and evaluations away from workers lacking free memory for the predicted peak. Earlier builds are the source of this prediction. `services.gradient.worker.build.metrics` can record the per-build measurements behind this prediction.

## Zones

Workers in a single datacenter can share a zone label, `services.gradient.worker.zone`. By default, workers carry no label. A [cluster job](../contributors/scheduler/clusters.md) that needs a fast interconnect places all of its members in one zone. Workers without a label together form one unnamed zone.

`services.gradient.worker.endpoint` specifies the address at which cluster peers can reach a worker. At cluster start, the server hands every member the endpoints of all other members.

## Access

| Kind | Registered | Serving |
|---|---|---|
| Project worker | Under one project | That project |
| Team worker | Under one [team](teams.md) | Every project granting the team's workers |
| Local worker | On the server host, automatically | Every new project, as a worker of the state-declared team `server` |

Workers only receive jobs from projects with a cache subscription.

## Team Workers

Each [team](teams.md) can have workers. A team worker works with a single token for the entire team. Projects that granted the team get read-only access to these workers.

Manage team workers on the team's **Workers** page.

| Setting | Effect |
|---|---|
| Grant with workers | Team worker active in the project |
| `new_projects.workers` | Team workers granted to every new project |
| `enabled` | Global switch for the worker in every granted project |

A worker ID can point to a team worker or to a set of project registrations. Never to both at once.

## Worker Stop

Workers can stop at any time. This include throwaway VMs. A stop never lose a job.

- The stopping worker will announce draining. The server will then send no new jobs.
- The worker will abort its running jobs.
- The server will queue those jobs again for other workers.

## Related

- [Quick Start](../get-started/quick-start.md): run a worker locally
- [Add a Remote Worker](../guides/remote-worker.md): hook up extra build machines
- [Evaluations and Builds](evaluations-and-builds.md): the work a worker performs
