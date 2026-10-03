# Eval Benchmark

`gradient-evalbench` is measuring an evaluation's time split between the server and the eval worker. The benchmark is a NixOS VM test evaluating the e2e hello flake four times. Each pass is leaving one capture bundle, to open in a viewer and drill into.

```mermaid
flowchart LR
  server["server VM: server, Postgres"] <-- "/proto" --> worker["worker VM: worker, eval subprocess"]
  server --> bundle["$out/&lt;run&gt;/"]
  worker --> bundle
```

## Running

```sh
nix build .#gradient-evalbench -L
cat result/summary.txt
```

- Not part of `nix flake check`. Perf and strace make the benchmark slow and noisy.
- A Gradient-built benchmark is offering four downloads on the evaluation page through `nix-support/hydra-build-products`.
- The downloads are `evalbench.tar.gz` (the whole bundle), `summary.txt`, `summary.json` and `index.html` (the [report](#report)).
- The builder is needing the `kvm` and `nixos-test` system features.
- The summarizer is coming with a cheap check of its own, `nix build .#checks.x86_64-linux.evalbench-summarize`.

## Benchmark Passes

| Pass | State before | Captured data |
|---|---|---|
| `cold-clean` | Fresh `gradient` database, empty worker eval cache | Spans, pcaps, Postgres statistics |
| `warm-clean` | Same commit evaluated again | Spans, pcaps, Postgres statistics |
| `cold-instrumented` | Reset as for `cold-clean` | Spans, perf, strace, `auto_explain` |
| `warm-instrumented` | Same commit evaluated again | Spans, perf, strace, `auto_explain` |

- A cold reset is dropping and recreating the database. The server is re-provisioning the database from `services.gradient.state`.
- The worker's Nix store is keeping fetched sources.
- Captures are active from the manual evaluation until the evaluation is leaving `EvaluatingDerivation`.
- The pass is then waiting for `Completed` before the next pass is starting.
- `summary.txt` is reporting the clean passes only. Perf, strace and `auto_explain` distort timings.
- The test is failing only on a failed evaluation or a missing capture file. There are no timing thresholds.

## Report

`gradient-evalbench-inspector` is rendering a bundle as one self-contained HTML page, and each chart as its own SVG next to the page. The benchmark is rendering its own bundle into `result/report/`. A downloaded bundle is renderable the same way.

```sh
nix run .#gradient-evalbench-inspector -- evalbench.tar.gz -o report
xdg-open report/index.html
```

- All passes: mode, evaluated seconds, traced wall time, `evaluation_metric`, and span totals side by side.
- Per pass, assignment: the assignment path (each step from the evaluate request to the worker's `job`, with the wait before each step).
- Per pass, jobs: job phase totals and a phase Gantt per assigned job.
- Per pass, spans: a span timeline per process lane, a span flame graph (spans folded by nesting) and span totals.
- Per pass, database: `pg_stat_statements`, the `auto_explain` statements by total plan time and the slowest plans.
- Per pass, CPU: the perf flame graphs.
- Every bar is carrying its details as a hover title.
- `<run>/trace.json` is copied beside the page for Perfetto.

## Bundle

| Path | Open with | Contents |
|---|---|---|
| `summary.txt`, `summary.json` | Any editor | Per pass: wall time, worker clock offset, `evaluation_metric`, spans by total time |
| `<run>/trace.json` | [ui.perfetto.dev](https://ui.perfetto.dev) | Server, worker and eval subprocess on one timeline |
| `<run>/trace/*.jsonl` | `jq` | Raw spans, one file per process |
| `<run>/proto.pcap` | Wireshark | Worker to server `/proto` traffic: round trips, frame sizes |
| `<run>/pg.pcap` | Wireshark (`pgsql` dissector) | Every statement of the server and its latency |
| `<run>/pg/pg_stat_statements.json` | `jq` | Top 100 statements of the `gradient` role by total time |
| `<run>/pg/job_phases.json` | `jq` | The worker's reported job phases (`dispatched_job_phase`) |
| `<run>/pg/auto_explain.log` | Any editor | Plans with `ANALYZE` of every statement, without per-node timing (instrumented passes) |
| `<run>/perf.data`, `<run>/flame.svg` | `perf report`, a browser | Server VM CPU profile (instrumented passes) |
| `<run>/worker/perf.data`, `<run>/worker/flame.svg` | `perf report`, a browser | Worker VM CPU profile, eval subprocess included |
| `<run>/worker/strace/worker.<pid>` | Any editor | Syscalls with timestamps and durations, one file per thread and child |
| `<run>/run.json` | `jq` | Evaluation id and seconds until the evaluation left `EvaluatingDerivation` |

## Spans

The benchmark is setting `services.gradient.log.traceDir` and `services.gradient.worker.log.traceDir` ([Configuration](../reference/configuration.md)). Each process is writing every closed `gradient*` span at `debug` or above as one JSON line. The console log level has no effect on these lines.

```json
{"name":"flush","target":"gradient_graph::writer","ts_us":1759140000123456,"dur_us":8123,"pid":812,"process":"server","fields":{"batches":2,"rows":100}}
```

| Process | Spans |
|---|---|
| `server` | `assign_queued_evals`, `offer_jobs`, `on_request_job_chunk`, `record_scores`, `on_request_job`, `request_job`, `claim_dispatch`, `send_credentials`, `assign_job`, `job_event` (`kind`, `queue_wait_us`), `handle_eval_result`, `assess_cached`, `record`, `flush`, `record_one`, `commit_one`, `transact_once`, `apply_batch` and one span per batch step, `after_commit`, `known_derivations`, `eval_stream_completed` |
| `worker` | `on_job_offer`, `score_candidates`, `send_scores`, `request_job`, `job`, `fetch_repository` (`clone_and_checkout`, `run_input_update`, `fetch_inputs`, `missing_paths`), `evaluate_flake`, `evaluate_derivations`, `wave`, `parse_drv_wave`, `query_known_derivations`, `report_eval_result` |
| `eval` | `open`, `lock_flake`, `discover`, `plan_shards`, `resolve` (`attr`) |

- `record` minus its `flush` is the batch's wait in the graph writer's mailbox.
- `job_event.queue_wait_us` is the report's wait behind earlier reports of the same worker.

## Clock Alignment

Server and worker are running on separate VMs with separate clocks. `summarize.py` is moving worker and eval spans onto the server clock.

- Upstream pairs: the n-th `report_eval_result` of a job against the n-th `job_event` with `kind = eval_result` of the same job (receive time = start minus `queue_wait_us`).
- Downstream pairs: `assign_job` end against the worker's `job` start.
- Offset = (minimum upstream delay - minimum downstream delay) / 2. This offset is making the fastest message in each direction equally slow.
- A pass without pairs in both directions is staying unaligned (`offset 0`, marked `unaligned`).
