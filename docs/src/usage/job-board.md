<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>

SPDX-License-Identifier: AGPL-3.0-only
-->

# Job Board

The **Job Board** (header tab, authenticated users) surfaces what the scheduler
is doing in real time and over history: live dispatched jobs with their scoring
breakdown, connected workers, throughput, and the most expensive builds.

## Pages

- **Overview** - live KPIs (connected workers, pending/active jobs, dispatched count) and builds-completed-per-hour.
- **Live Jobs** - the in-flight dispatched jobs you can see, updated live over a WebSocket. Click a persisted job to open its **inspection page** (`/board/jobs/{id}`): the server marks (queued, ready, dispatched, finished), the **worker timeline** described below, the per-rule scoring breakdown with contribution bars, and the job/worker context captured at dispatch time. Jobs in projects you can't access are shown only as an aggregate count.
- **Scheduler** - wait breakdown (**queue wait excluding dependency wait** vs dependency wait) plus an aggregate scoring view: score-distribution histogram and mean per-rule contribution over recent dispatches (`GET /api/v1/board/scoring/summary`). The **?** next to a rule name opens a popup explaining what that rule rewards or penalizes, served by `GET /api/v1/board/scoring/rules`.
- **Throughput** - build pipeline (created/completed/failed) and evaluation rates per hour, plus active jobs per worker.
- **Durations** - build-duration trend (avg vs max) and the queue-vs-dependency wait split.
- **Workers** - fleet over time (connected vs draining), capability trend, load by **capability** and **architecture** (paired radars) plus load by **feature** (bar), per-worker slot utilisation, and the live worker table. Each load chart plots busy % as the in-flight jobs of that kind against the summed slot capacity of the workers that can serve it (`GET /api/v1/board/workers/load`), so an operator can tell whether the fleet is eval-, build-, or architecture-bound and which architecture/feature needs more workers.
- **Cache** - cache totals, traffic, and storage-growth series (`GET /api/v1/board/cache`), plus per-upstream latency. Upstream metrics are keyed by URL, so the same URL registered under several caches/projects shows as one series.
- **Network** - NAR egress, per-worker network/disk speeds, and a per-route HTTP latency/throughput table (`GET /api/v1/board/network`).
- **Jobs** - tabbed rankings of the costliest builds in a window: longest wall-clock, **peak RAM**, **CPU time**, **disk I/O** (all per-build via cgroup v2), and **network** (host-level peak during the build window - cgroup v2 has no per-build network), plus top-projects-by-build-time for superusers.
- **System Health** (superuser) - process/runtime snapshot, rollup-pipeline lag, and HTTP route stats (`GET /api/v1/board/health`). Also exposes **Run Deep GC** and an **Enable/Disable Draining** toggle (`POST /api/v1/admin/draining`): while draining, the scheduler stops dispatching and parks every in-flight evaluation so the server can be stopped safely; it clears automatically on the next startup.

## The worker timeline

The server's own marks say when a job was queued, became ready, was dispatched
and finished. They say nothing about what the worker did in between. Every job
therefore records a timeline of nested phase spans and sends it back inside its
terminal message; the job inspection page draws it as a horizontal chart with a
per-phase table (duration, share of total, paths and bytes moved).

Offsets are milliseconds from the moment the worker accepted the job, never from
the enclosing span. A phase opened inside another is drawn underneath it, so a
NAR push sits under the compress that started it.

| Phase | Meaning |
| --- | --- |
| `fetch` | Cloning or archiving the flake source. |
| `push_inputs` | Uploading the archived source's paths to the cache. |
| `eval_flake` | Evaluating the flake outputs. |
| `eval_derivations` | Resolving the outputs to derivations. |
| `eval_cache_pull` | Waiting for the shared eval-cache blob. |
| `eval_cache_push` | Handing the eval-cache blob back. |
| `known_derivations_wait` | Waiting for the server to say which `.drv`s it already knows. |
| `drv_closure_push` | Pushing a batch's `.drv` runtime closure. |
| `prefetch` | Importing a build's cache-resident inputs. |
| `substitute_relay` | Relaying an output that an upstream cache already has. |
| `build` | One derivation build. |
| `compress` | The post-build compress and push loop. |
| `nar_push` | One output NAR upload; nested under `compress`. |
| `cache_query_wait` | Waiting for a cache-status reply. |

A job records at most 2000 spans. A large evaluation pushes one NAR per closure
member, so past that ceiling the phases still run, they just stop being timed
individually rather than growing the message and the table without bound.

Timelines feed two other things: the three per-phase columns on
`evaluation_metric` are summed from the `fetch`, `eval_flake` and
`eval_derivations` spans, and every span rolls up as
`phase.<kind>.<phase>.ms` for the metrics query API.

Per-worker deep metrics (CPU/RAM/disk/network time-series, connection history) live under **Project → Workers → Metrics** (`GET /api/v1/projects/{project}/workers/{worker_id}/metrics`).

## Visibility

Data is masked to the caller's scope:

- **Superusers** see every project and all worker/infrastructure detail.
- **Members** see their projects (plus public projects) in full; cross-project infrastructure is anonymized (counts only, no foreign identities).
- **Anonymous** callers see public-project aggregates only.

## Data sources

The board reads from dedicated tables populated as the scheduler runs:

- `dispatched_job` - one row per dispatch with the winning score, per-rule breakdown, and job/worker context (the scoring-debug substrate).
- `phase_event` + per-phase timestamp columns on `build`/`evaluation` - accurate phase timing. The build lifecycle is `created_at → queued_at → ready_at → dispatched_at → build_started_at → build_finished_at`, where `queued_at→ready_at` is **dependency wait** (`deps.wait_ms`) and `ready_at→dispatched_at` is **queue wait excluding dependency wait** (`dispatch.wait_ms`).
- `dispatched_job_phase` - one row per worker phase span, nested via `parent_seq` and cascade-deleted with its job. Written when the job reports its terminal message, alongside `dispatched_job.finished_at` and `outcome`.
- `worker_connection` / `worker_sample` - worker sessions and a periodic live-metric time-series.
- `derivation_metric` - per-build resource usage captured by the worker from the build's cgroup (peak RAM, CPU time, disk read/write, OOM) plus a host network peak; powers the Expensive Jobs resource tabs. Requires cgroup metrics enabled on the worker.
- `metric_rollup` - time-bucketed aggregates (minute → hour → day → week) produced by a background aggregator, queried via `GET /api/v1/metrics/query` (catalog at `GET /api/v1/metrics/catalog`).

Retention and aggregation intervals are configurable - see [Configuration](../configuration.md#metrics-pipeline--retention).
