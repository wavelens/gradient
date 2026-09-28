# Job Board

What the scheduler and the workers do right now and over time: live jobs, why a job went to a worker, fleet load and the most expensive builds. **Job Board** in the header.

![Job Board overview](../assets/screenshots/job_board_overview.png)

| Tab | Shows | Actions |
|---|---|---|
| Overview | Connected workers, pending and active jobs, builds per hour | |
| Live Jobs | Jobs running now, updated live | Open a job's inspection page |
| Scheduler | Queue wait against dependency wait, score distribution, mean contribution per scoring rule | **?** next to a rule explains the rule |
| Throughput | Builds created, completed and failed, evaluations per hour, active jobs per worker | |
| Durations | Average and maximum build time, the wait split over time | |
| Workers | Fleet size, load by capability, system and feature, slot use per worker | Spot the missing kind of worker |
| Cache | Stored size, traffic, growth, latency per upstream | |
| Network | NAR egress, worker network and disk speed, HTTP latency per route | |
| Jobs | The costliest builds by wall time, peak RAM, CPU time, disk I/O and network | Pick the time window |
| Evals | The costliest evaluations by time, peak memory, thunks, function calls and allocations | Pick the time window |
| System Health | Server runtime, metric pipeline lag, route stats; superusers only | **Run Deep GC**; **Enable Draining** before stopping the server |

Draining stops new dispatches and parks running evaluations for a safe server stop; the next start clears the flag.

## Job Inspection

![Job score breakdown](../assets/screenshots/job_board_job_score.png)

A job from **Live Jobs** opens with:

- The server's marks: queued, ready, dispatched, finished.
- The worker timeline: nested phases (fetch, evaluate, build, compress, NAR push) with duration, share and bytes moved.
- The score breakdown: each scoring rule's contribution to the winning worker.

The timeline shows where a slow job spent its time, the score shows why the job landed on that worker.

??? note "Timeline Phases"
    | Phase | Meaning |
    |---|---|
    | `fetch` | Cloning or archiving the flake source |
    | `push_inputs` | Uploading the archived source to the cache |
    | `eval_flake` | Evaluating the flake outputs |
    | `eval_derivations` | Resolving the outputs to derivations |
    | `eval_cache_pull`, `eval_cache_push` | Waiting for and returning the shared evaluation cache |
    | `known_derivations_wait` | Waiting for the server to name the `.drv` files it already knows |
    | `drv_closure_push` | Pushing a batch of `.drv` closures |
    | `prefetch` | Importing a build's inputs from the cache |
    | `substitute_fetch` | Fetching one output from an upstream |
    | `download` | One `builtin:fetchurl`, run by the worker without Nix |
    | `build` | One derivation build |
    | `compress` | Packing the job's outputs for upload |
    | `nar_push` | One output upload, nested under `compress` |
    | `cache_query_wait` | Waiting for a cache status reply |

    A job records at most 2000 phases; past that, phases still run but are no longer timed one by one.

## Workers

![Worker load](../assets/screenshots/job_board_workers.png)

Each load chart sets the running jobs of one kind against the slots of the workers that serve that kind. A chart near 100% names the worker type to add: evaluation, build, a system such as `aarch64-linux`, or a feature such as `kvm`.

Per-worker CPU, memory, disk and network history lives under **Project -> Workers -> Metrics**.

## Visibility

| Viewer | Sees |
|---|---|
| Superuser | Everything |
| Member | Own and public projects in full; other projects' infrastructure only as counts |
| Signed out | Public project totals |

The resource tabs need build metrics on the workers, see [`services.gradient.worker.build.metrics`](../reference/configuration.md#workerbuild). Retention is set under [metrics pipeline and retention](../reference/configuration.md#metrics).

## Related

- [Workers](../concepts/workers.md): capabilities and matching builds
- [Scheduler Policies](../reference/scheduler-policies.md): the scoring rules
