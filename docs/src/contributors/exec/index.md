# Exec Jobs

Exec jobs start a flake app on a worker once the program is in the cache. Apps turn into entry points during evaluation. Jobs wait for the program build and then go to a matching worker. The user side is in [Run Exec Jobs](../../guides/exec-jobs.md).

```mermaid
flowchart LR
    app["App with passthru.gradient"] --> ep["Entry point"]
    ep --> created["Created"]
    created --> queued["Queued"]
    queued --> starting["Starting"]
    starting --> running["Running"]
    running --> done["Completed, Failed or Aborted"]
```

## Crate Map

| Crate | Part |
|---|---|
| `gradient-wire` | `ExecSpec`, `ExecRequirement`, `ExecInventory`, device strings, defaults and caps |
| `gradient-types` | `gradient_types::exec`, wait reasons, `exec_job.*` events, the `exec.enable` option |
| `gradient-eval` | The app leaf in the flake walk and `EXEC_APP_EXPR` |
| `gradient-worker` | Conversion of the eval result into an `ExecSpec` (`worker_pool::exec_app`) |
| `gradient-db` | Every exec statement under `gradient_db::exec_jobs` |
| `gradient-graph` | Every exec job transition and event |
| `gradient-pool` | Matching (`WorkerCaps::fits_exec`) and device holds |
| `gradient-scheduler` | Loading, placement, dispatch, wait reasons and pre-warm counts |
| `gradient-proto` | Phases, program start, logs and the inventory on the session |
| `gradient-web` | `/exec-jobs` endpoints and the project switch |

## Entry Points

- Attrsets with `type = "app"` and `passthru.gradient` are walk leaves. Other apps stay opaque.
- `EXEC_APP_EXPR` can read the block. Apps without exactly one derivation output in the string context of `program` fail with a message.
- The eval cache can resolve `image` to `config.system.build.toplevel`. Images get a second entry point, `<attr>.passthru.gradient.image`.
- Specs live as JSON in `entry_point.exec`. Exec entry points reference the program derivation in `entry_point.derivation`.
- The graph writer can refuse an entry point before insertion, with a message.

| Message | Check |
|---|---|
| `exec jobs are disabled on this instance` | `exec.enable` off, checked first |
| `exec jobs are disabled for this project` | `project.exec_enabled` off |
| `exec jobs with more than one node are not supported yet` | `nodes > 1` |

- Refused programs also lose the image entry point, even in a later batch.
- Restarts copy `entry_point.exec` along with the entry point.

## Placement

- `ExecSpec::requirement_on` can build the `ExecRequirement` from the spec and the worker `defaults`. Defaults only fill fields the app left out.
- `ExecInventory.allow` can restrict every field. Requirements outside `allow` make the worker unfit, without a downgrade.
- `ExecSpec::limits_on` can clamp `timeout`, `maxSilent` and `retries` to the caps of `allow` at dispatch.
- Placed jobs hold their devices until the end of the attempt. Held devices are never free, even before the next `WorkerMetrics`.
- Exec jobs skip the RAM fit rule. Queue age orders the matching workers.
- Only workers without draining and with access to the job's project count, for placement and for wait reasons.
- `WaitingReason::ExecWorkers` can name the unmet labels, devices, mounts, node count or custom image.

## Pre-Warm Counts

Workers get `ServerMessage::PreWarmVms { count }` as an absolute target of guests to boot ahead.

- Upcoming jobs are exec entry points of a running evaluation without a job yet, plus jobs in `Created` or `Queued`.
- Jobs with a custom image never count.
- Upcoming jobs add `nodes` to every worker without draining, with a fit against device totals and with access to the job's project.
- The scheduler can refresh the counts after new capabilities, an exec pass, a recorded eval result and an abort.
- Counts go out only on change. New connections start at 0.

## Lifecycle

| Status | Meaning |
|---|---|
| `Created` | Waiting for the program (and image) build |
| `Queued` | Builds cached, waiting for a worker |
| `Starting` | Attempt open, program not started yet |
| `Running` | The worker reported `ProgramStarted` |
| `Completed`, `Failed`, `Aborted` | Final |

- Dispatches open an attempt in `exec_job_attempt`. Attempt end reasons are `Completed`, `Failed`, `Timeout`, `Lost` and `Aborted`.
- `Failed` and `Timeout` lead to a restart while `retries_left > 0`, one retry per restart.
- `Lost` attempts never use a retry. Ten losses in a row fail the job.
- Aborted jobs never restart. Late reports for a closed attempt change nothing.
- Status changes emit `exec_job.queued`, `started`, `phase`, `completed`, `failed` or `aborted`, see [Events and Webhooks](../../reference/events.md).
- Logs share the build log storage per attempt ID. The deep GC can keep the log of every exec attempt.
