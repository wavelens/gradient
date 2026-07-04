<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>

SPDX-License-Identifier: AGPL-3.0-only
-->

# Job Board: per-capability / per-feature / per-architecture worker load metrics (#417)

## Problem

1. **Load by capability is wrong.** `workers.component.ts::loadRadar()` divides each
   worker's *total* `assigned_jobs` by capacity for all three capabilities, so a worker
   that does eval + fetch + build reports the same busy % on every axis. It cannot answer
   "am I build-bound or eval-bound?".
2. **No per-architecture / per-feature load.** An operator cannot see which architecture or
   system feature is the bottleneck ("which do I need more workers for?").
3. **Cache latency graph is double-boxed.** Each upstream latency chart sits in a `.upstream`
   card wrapping the already-carded `.metric-chart`, unlike every other graph.
4. **Upstream metrics never merge by URL.** `upstream_metric` is keyed by `CacheUpstreamId`,
   so the same upstream URL registered under several caches/orgs produces separate series.

## Root cause of 1 & 2

`WorkerShared.assigned_jobs` is only a `HashSet<String>` of job IDs — no per-job kind, arch,
or features. The authoritative per-job data lives in the scheduler's `JobTracker`
(`PendingJob::{Eval,Build}`, with `architecture` + `required_features` on build jobs, and
`FlakeTask`s on eval jobs). A worker advertises many architectures but each build targets
exactly one, so per-arch/per-feature attribution **must** be aggregated server-side from the
job tracker; it cannot be derived from per-worker aggregates on the client.

## Design

### Part 1 + 2 — Load breakdowns

New scheduler method `board_worker_load()` reads the worker pool and job tracker under their
locks and returns three breakdowns. Each is a list of buckets:

```rust
pub struct LoadBucket { pub key: String, pub in_flight: u32, pub capacity: u32, pub workers: u32 }
pub struct WorkerLoad {
    pub by_capability: Vec<LoadBucket>,   // keys: "eval", "fetch", "build"
    pub by_architecture: Vec<LoadBucket>, // keys: e.g. "x86_64-linux"
    pub by_feature: Vec<LoadBucket>,      // keys: e.g. "kvm", "big-parallel"
}
```

- **capacity** = Σ `max_concurrent_builds` over workers that *have* that capability / arch /
  feature. A worker counts toward every bucket it can serve (intentional overlap: each bucket
  answers "how loaded is the capacity that can serve this kind of work").
- **in_flight** = active jobs classified from the job tracker:
  - Build job → `build` bucket, its `architecture` bucket, and each `required_features` bucket.
  - Flake (eval) job → `eval` and/or `fetch` bucket depending on its `FlakeTask`s (a job with
    both tasks counts to both).
- busy % (computed client-side) = `round(100 * in_flight / capacity)`; `0` when capacity is 0.
  Bounded to [0,100] because a job of a given kind only runs on capacity that can serve it.

Because scoping needs the caller identity, aggregation lives in the web handler: the scheduler
exposes raw state (existing `board_workers()` → `WorkerInfo` plus a new
`board_active_jobs()` → `{ worker_id, org, kind, architecture, required_features, eval_task,
fetch_task }`), and the handler filters to the caller's accessible orgs via the existing
`MetricsScope` before summing.

**Endpoint:** new `GET /api/v1/board/workers/load` → `WorkerLoad` (dedicated, matching the
granular board-endpoint style; the `/board/workers` table response is unchanged).

**Frontend:** `board.service.ts::getWorkerLoad()` + typed interfaces; `workers.component.ts`
renders three `app-metric-chart type="radar"` charts (capability, architecture, feature) from
raw in_flight/capacity, computing busy % per axis.

*Scope note:* moving aggregation server-side and scoping it means foreign-org workers no longer
contribute to a member's radar (they leaked capability booleans before). No change for
superusers, who see the whole fleet.

### Part 3 — Un-box cache latency

In `cache.component.ts`, drop the `.upstream` card wrapper and render each upstream's latency as
a single plain `app-metric-chart`, folding the `<ms> · <hit>% hit · <n> req` meta into the chart
title so it matches every other graph.

### Part 4 — Merge upstream metrics by URL (storage-level)

Re-key the metric on the URL, which is the true identity of an upstream cache.

- **Migration `m20260705_000000_upstream_metric_by_url`:**
  1. add `upstream_url TEXT` to `upstream_metric`;
  2. backfill `upstream_url` from `cache_upstream.url` via the FK;
  3. merge colliding `(upstream_url, bucket_time)` rows (sum `latency_ms_sum`, `request_count`,
     `narinfo_hits`, `narinfo_misses`);
  4. delete rows whose url is null, drop the `cache_upstream` FK, drop the old unique index and
     the `upstream` column, set `upstream_url NOT NULL`, add unique `(upstream_url, bucket_time)`;
  5. re-scope existing `metric_rollup` `upstream.*` rows from `{'upstream': id}` to
     `{'upstream_url': url}` and merge per `(metric, granularity, bucket_start, url)` — min/max/
     sum_sq are 0 for these metrics, so the merge is a clean additive sum of `count`/`sum`, with
     `scope_hash = hashtextextended(url, 0)`.
- **Writer:** `extend_with_upstream_results` folds `probe_batch` stats (keyed by upstream id)
  into a `HashMap<normalized_url, UpstreamAccum>` using the endpoints' id→url map;
  `upsert_upstream_metrics` upserts on `(upstream_url, bucket_time)`. Upstreams without a URL are
  skipped.
- **Rollup:** `upstream_{latency,hits,misses}_sql` scope `{'upstream_url': upstream_url}`, group
  by `upstream_url`.
- **Read:** `get_board_upstreams` aggregates by url, org-scopes to URLs configured in the
  caller's caches, sets `display_name` to the URL host, and masks the full `url` for
  non-superusers. Same-URL upstreams collapse into one series.

## Testing (TDD)

- Scheduler unit test: `board_worker_load` over a small fixture (2–3 workers with overlapping
  capabilities/arches + a few active build/eval jobs) asserts expected `in_flight`/`capacity`
  per bucket, including per-capability divergence and per-arch attribution.
- DB test: same-URL upstreams merge into one `upstream_metric` row per bucket after the writer
  change.
- Rely on CI for the full suite and migration verification (no local `cargo test`); verify
  locally with `cargo clippy`.
- Frontend: extend the workers component spec for the three computed radar series if a spec
  harness exists.

## Docs

- `docs/gradient-api.yaml`: add `GET /board/workers/load`; note the reshaped upstream response.
- `docs/src/usage/job-board.md`: describe the three load radars and the by-URL upstream merge.
- `docs/src/tests.md`: record the new tests.
- No new environment variables → no `nix/modules` or configuration doc changes.

## Decisions

- Dedicated `GET /board/workers/load` rather than reshaping `/board/workers`.
- Radar (not bars) for arch/feature, for visual consistency with the capability radar.
- Storage-level by-URL re-keying (migration), not a read-time merge — URL is the storage
  identity.
- Merged-URL `display_name` = URL host; per-instance custom names cannot survive a merge.
