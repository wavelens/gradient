<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Eval Benchmark

`gradient-evalbench` can measure an evaluation's time split between the server and the eval worker. The benchmark is a NixOS VM test evaluating the e2e hello flake four times. Each pass will leave one capture bundle, to open in a viewer and drill into.

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
- A Gradient-built benchmark will offer four downloads on the evaluation page through its `nix-support/hydra-build-products` file.
- The downloads are `evalbench.tar.gz` (the whole bundle), `summary.txt`, `summary.json` and `index.html` (the [report](#report)).
- The builder must have the `kvm` and `nixos-test` system features.
- The summarizer has a cheap check of its own, `nix build .#checks.x86_64-linux.evalbench-summarize`.

## Benchmark Passes

| Pass | State before | Captured data |
|---|---|---|
| `cold-clean` | Fresh `gradient` database, empty worker eval cache | Spans, pcaps, Postgres statistics |
| `warm-clean` | Same commit evaluated again | Spans, pcaps, Postgres statistics |
| `cold-instrumented` | Reset as for `cold-clean` | Spans, perf, strace, `auto_explain` |
| `warm-instrumented` | Same commit evaluated again | Spans, perf, strace, `auto_explain` |

- A cold reset will drop and recreate the database. The server will then re-provision the database from `services.gradient.state` again.
- The worker's Nix store will keep fetched sources.
- The worker VM can boot with the nixpkgs source already in its store. Cold passes then measure Gradient, not a copy of nixpkgs into the VM.
- Captures are active from the manual evaluation until the evaluation has left the `EvaluatingDerivation` status.
- The pass will then wait for `Completed` before the next pass can start.
- `summary.txt` will only report on the `cold-clean` and `warm-clean` pass. Perf, strace and `auto_explain` distort timings.
- The test can fail only on a failed evaluation or a missing capture file. There are no timing thresholds.

## Report

`gradient-evalbench-inspector` can render a bundle as one self-contained HTML page, and each chart as its own SVG next to the page. The benchmark will render its own bundle into the `result/report/` directory. A downloaded bundle is renderable the same way.

```sh
nix run .#gradient-evalbench-inspector -- evalbench.tar.gz -o report
xdg-open report/index.html
```

- **All passes:** Mode, evaluated seconds, traced wall time, `evaluation_metric`, and span totals side by side.
- **Assignment per pass:** The assignment path, with each step from the evaluate request to the worker's `job` span and the wait before each step.
- **Jobs per pass:** Job phase totals and a phase Gantt per assigned job.
- **Spans per pass:** A span timeline per process lane, a span flame graph folded by nesting, and span totals.
- **Database per pass:** The `pg_stat_statements` view, the `auto_explain` statements by total plan time and the slowest plans.
- **CPU per pass:** The perf flame graphs.
- Every bar will carry its details as a hover title.
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

The benchmark will set `services.gradient.log.traceDir` and `services.gradient.worker.log.traceDir` ([Configuration](../reference/configuration.md)). Each process can then write every closed `gradient*` span at `debug` or above as one JSON line. The console log level has no effect on these lines.

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

Server and worker are running on separate VMs with separate clocks. `summarize.py` will move the spans of the worker and the eval subprocess onto the server clock.

- Upstream pairs: the n-th `report_eval_result` of a job against the n-th `job_event` with `kind = eval_result` of the same job (receive time = start minus `queue_wait_us`).
- Downstream pairs: `assign_job` end against the worker's `job` start.
- Offset = (minimum upstream delay - minimum downstream delay) / 2. This offset will make the fastest message in each direction equally slow.
- A pass without pairs in both directions will stay unaligned (`offset 0`, marked `unaligned`).
