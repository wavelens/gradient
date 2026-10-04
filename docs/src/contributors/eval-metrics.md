# Evaluation Metrics

Evaluation workers record Nix metrics per evaluation, the same way build jobs record resource metrics. The numbers are feeding the Job Board's **Evals** tab. They are also routing RAM-heavy evaluations to big machines.

```mermaid
flowchart LR
    sub[eval subprocess] -->|stats delta per request| worker[Worker]
    worker -->|EvalStats| server[Server]
    server --> tables[(evaluation_metric,<br/>evaluation_attr_cost,<br/>flake_output_node)]
    tables --> board[Job Board: Evals]
    tables --> rule[ResourceFitRule: p95 RAM per task]
```

## Tables

| Table | Contents |
|---|---|
| `evaluation_metric` | Per evaluation: thunks, function calls, primop calls, lookups, allocated bytes, peak GC heap and RSS (MB), `total_eval_ms`, `worker_id`, and the phase columns `fetch_ms`, `eval_flake_ms`, `eval_drv_ms` |
| `evaluation_attr_cost` | Per entry point: thunks, function calls, wall clock and allocated bytes, bucketed by the wildcard target |
| `flake_output_node` | The walked flake-output tree: `path`, `parent`, `name`, `kind`, `is_derivation`, `drv_path` |

- The phase columns are not coming from the Nix stats.
- The phase columns hold the sum of the job timeline's `fetch`, `eval_flake` and `eval_derivations` spans. The server will compute them on the job's terminal message.
- The phase columns are 0 between `EvalStats` and completion. They stay 0 when the worker vanished before reporting. Phase names are on the [Job Board](../ui/job-board.md#job-inspection).
- Entry-point costs are aggregated in the resolver on each completed request.
- `flake_output_node` can only record the nodes visited by the discovery walk. Nothing extra is evaluated.
- The frontend will render the rows as a `nix flake show`-like tree.

## RAM Routing

`ResourceFitRule` (see [Scoring](scheduler/scoring.md)) can read a per-task rolling p95 of `peak_rss_mb` over the last 24 h. Tasks whose evaluations needed much RAM go to big-RAM workers. The prediction can update as evaluations finish, with no manual thresholds.

## Endpoints

| Endpoint | Response |
|---|---|
| `GET /api/v1/board/evals/expensive-by-resource?metric=...&window_days=N` | Top evaluations by `time`, `rss`, `heap`, `thunks`, `fncalls` or `alloc`, project-scoped. `metric` is matched against a closed list |
| `GET /api/v1/evals/{evaluation}/flake-graph` | The walked flake-output tree of one evaluation |

## Overhead

- `GRADIENT_WORKER_EVAL_METRICS` (default `true`) can switch capture off. The value `false` will skip the stats read entirely.
- The enabled cost is one cumulative-counter read per resolver request, diffed per worker. No `--count-calls`-style instrumentation.
