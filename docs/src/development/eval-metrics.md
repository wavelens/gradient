# Evaluation Metrics

Eval-workers capture per-evaluation Nix metrics and persist them, mirroring how
builds capture resource metrics. The data drives the Job Board's "Expensive
Evals" panel and self-tunes the scheduler so heavy evaluations land on big-RAM
machines.

## What's captured

Three tables are written per evaluation:

- **`evaluation_metric`** — per-eval aggregate: total thunks, function calls,
  primop calls, lookups, allocated bytes, peak GC heap (MB), peak RSS (MB), the
  per-phase wall-clock (`fetch_ms`, `eval_flake_ms`, `eval_drv_ms`,
  `total_eval_ms`) and the `worker_id` that ran it.
- **`evaluation_attr_cost`** — per-entry-point hotspots: thunks, function calls,
  eval wall-clock and allocated bytes bucketed by user entry-point.
- **`flake_output_node`** — the walked flake-output subgraph: `path`, `parent`,
  `name`, `kind`, `is_derivation`, `drv_path`.

## Per-entry-point hotspots

Costs are bucketed by the eval's wildcard target (the user entry-point), so you
can see which outputs are expensive to evaluate rather than just the eval total.
The aggregation runs in the resolver as each request completes.

## Walked flake graph

The discovery BFS records every flake output it actually walked — no extra
evaluation is forced. The subgraph is stored as `flake_output_node` rows and
served back as a `nix flake show`-like tree for frontend rendering.

## Self-tuning RAM-to-machine routing

A per-project rolling window takes the p95 of eval `peak_rss_mb` over the last
24h and feeds it to the scheduler's `ResourceFitRule` (see
[scheduler scoring](scheduler-scoring.md)). A project whose evaluations have
historically needed lots of RAM is routed to big-RAM eval machines, and the
prediction re-tunes itself as new evals complete — there are no manual
thresholds.

## Board endpoints

- `GET /board/evals/expensive-by-resource?metric={time,rss,heap,thunks,fncalls,alloc}&window_days=N`
  — the top evaluations by a resource, org-scoped. `metric` is matched against a
  closed allow-list, so it cannot inject SQL.
- `GET /evals/{evaluation}/flake-graph` — the walked flake-output graph for one
  evaluation.

Both are surfaced in the Job Board's "Expensive Evals" panel.

## Toggle and overhead

`GRADIENT_EVAL_METRICS_ENABLED` (worker-side, default `true`) gates capture.
When `false`, the eval-worker skips the per-request stats read entirely, so
there is zero added overhead. Even when enabled the overhead is one cheap
cumulative-counter read per resolver request, diffed per worker — there is no
`--count-calls`-style instrumentation.
