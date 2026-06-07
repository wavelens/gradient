<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>

SPDX-License-Identifier: AGPL-3.0-only
-->

# Job Board

The **Job Board** (header tab, authenticated users) surfaces what the scheduler
is doing in real time and over history: live dispatched jobs with their scoring
breakdown, connected workers, throughput, and the most expensive builds.

## Pages

- **Overview** — live KPIs (connected workers, pending/active jobs, dispatched count) and builds-completed-per-hour.
- **Live Jobs** — the in-flight dispatched jobs you can see, updated live over a WebSocket. Click a persisted job to open its **inspection page** (`/board/jobs/{id}`): the per-rule scoring breakdown with contribution bars, queue→dispatch wait, and the job/worker context captured at dispatch time. Jobs in orgs you can't access are shown only as an aggregate count.
- **Scheduler & Scoring** — wait breakdown (**queue wait excluding dependency wait** vs dependency wait) plus an aggregate scoring view: score-distribution histogram and mean per-rule contribution over recent dispatches (`GET /api/v1/board/scoring/summary`).
- **Throughput** — build pipeline (created/completed/failed) and evaluation rates per hour, plus active jobs per worker.
- **Durations** — build-duration trend (avg vs max) and the queue-vs-dependency wait split.
- **Workers** — fleet over time (connected vs draining), capability trend, load-by-capability radar, per-worker slot utilisation, and the live worker table.
- **Cache** — cache totals, traffic, and storage-growth series (`GET /api/v1/board/cache`).
- **Network & API** — NAR egress, per-worker network/disk speeds, and a per-route HTTP latency/throughput table (`GET /api/v1/board/network`).
- **Expensive Jobs** — the longest builds in a window, with an option to exclude acknowledged (muted) derivations.
- **System Health** (superuser) — process/runtime snapshot, rollup-pipeline lag, and HTTP route stats (`GET /api/v1/board/health`).

Per-worker deep metrics (CPU/RAM/disk/network time-series, connection history) live under **Organization → Workers → Metrics** (`GET /api/v1/orgs/{org}/workers/{worker_id}/metrics`).

## Visibility

Data is masked to the caller's scope:

- **Superusers** see every organization and all worker/infrastructure detail.
- **Members** see their organizations (plus public orgs) in full; cross-org infrastructure is anonymized (counts only, no foreign identities).
- **Anonymous** callers see public-org aggregates only.

## Data sources

The board reads from dedicated tables populated as the scheduler runs:

- `dispatched_job` — one row per dispatch with the winning score, per-rule breakdown, and job/worker context (the scoring-debug substrate).
- `phase_event` + per-phase timestamp columns on `build`/`evaluation` — accurate phase timing. The build lifecycle is `created_at → queued_at → ready_at → dispatched_at → build_started_at → build_finished_at`, where `queued_at→ready_at` is **dependency wait** (`deps.wait_ms`) and `ready_at→dispatched_at` is **queue wait excluding dependency wait** (`dispatch.wait_ms`).
- `worker_connection` / `worker_sample` — worker sessions and a periodic live-metric time-series.
- `metric_rollup` — time-bucketed aggregates (minute → hour → day → week) produced by a background aggregator, queried via `GET /api/v1/metrics/query` (catalog at `GET /api/v1/metrics/catalog`).

Retention and aggregation intervals are configurable — see [Configuration](../configuration.md#metrics-pipeline--retention).
