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
- **Live Jobs** — the in-flight dispatched jobs you can see, updated live over a WebSocket. Click a job to inspect its **per-rule scoring breakdown** and the job/worker context captured at dispatch time. Jobs in orgs you can't access are shown only as an aggregate count.
- **Workers** — connected workers by capability, plus per-worker load, CPU/RAM, and architectures.
- **Expensive Jobs** — the longest builds in a window, with an option to exclude acknowledged (muted) derivations.

## Visibility

Data is masked to the caller's scope:

- **Superusers** see every organization and all worker/infrastructure detail.
- **Members** see their organizations (plus public orgs) in full; cross-org infrastructure is anonymized (counts only, no foreign identities).
- **Anonymous** callers see public-org aggregates only.

## Data sources

The board reads from dedicated tables populated as the scheduler runs:

- `dispatched_job` — one row per dispatch with the winning score, per-rule breakdown, and job/worker context (the scoring-debug substrate).
- `phase_event` + per-phase timestamp columns on `build`/`evaluation` — accurate phase timing (queue → ready → dispatched → building → terminal).
- `worker_connection` / `worker_sample` — worker sessions and a periodic live-metric time-series.
- `metric_rollup` — time-bucketed aggregates (minute → hour → day → week) produced by a background aggregator, queried via `GET /api/v1/metrics/query` (catalog at `GET /api/v1/metrics/catalog`).

Retention and aggregation intervals are configurable — see [Configuration](../configuration.md#metrics-pipeline--retention).
