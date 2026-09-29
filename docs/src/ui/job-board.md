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
| Storage | NAR storage latency and errors per operation (file or S3), writer lane fill and send stalls, NAR serve queue and failures; minute resolution up to 6 h; superusers only | |

=== "Live Jobs"

    ![Live Jobs tab](../assets/screenshots/job_board_live.png)

=== "Scheduler"

    ![Scheduler tab](../assets/screenshots/job_board_scheduler.png)

=== "Jobs"

    ![Jobs tab](../assets/screenshots/job_board_expensive_jobs.png)

Draining stops new dispatches and parks running evaluations for a safe server stop; the next start clears the flag.

## Storage Metrics

The **Storage** tab reads these metric keys. Counts are summed per bucket; an empty bucket means zero events.

| Key | Labels | Unit | Chart |
|---|---|---|---|
| `storage.op_ms` | Operation: `get`, `head`, `put`, `put_part`, `multipart_create`, `multipart_complete`, `delete` | ms per call | Storage latency |
| `storage.op_errors` | `<op>/error`, `<op>/cancelled`, `read/error` | count | Storage errors |
| `proto.bulk_lane_fill` | `bulk` | peak fill 0-1, shown as % | Writer lanes |
| `proto.control_lane_fill` | `control` | peak fill 0-1, shown as % | Writer lanes |
| `proto.send_stalls` | `bulk`, `control` | count | Writer lanes (bars) |
| `nar.serves_waiting` | `waiting` | peak serves | NAR serves |
| `nar.serves_active` | `active` | peak serves | NAR serves |
| `nar.serve_failures` | `not_found`, `storage_timeout`, `storage_error`, `send_stalled` | count | NAR serves (bars) |

- A `cancelled` call was dropped before it finished, usually by a timeout on a hung backend.
- Lane fill and send stalls cover every protocol session, worker and cache alike; a stall is a send that waited past `send_chunk_timeout` for lane capacity.
- Build log objects on S3 go through the same store and share the storage operation labels.

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
