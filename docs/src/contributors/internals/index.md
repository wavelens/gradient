# Internals

Implementation details outside the scheduler and the protocol: how forge events enter, how NARs are stored and served, how the graph is queried in SQL, and how requests authenticate. Paths are relative to `backend/`.

```mermaid
flowchart LR
    forge[Forge] -->|webhook| hooks[Forge Webhooks]
    hooks --> eval[Evaluation]
    worker[Worker] -->|NAR| storage[NAR Storage]
    storage --> serving[Cache Serving]
    serving --> nix[nix clients]
    eval --> graph[(Graph)]
    graph --> queries[Graph Queries]
```

<div class="grid cards" markdown>

-   :material-webhook: **[Forge Webhooks](forge-webhooks.md)**

    Hook routes, signature checks and the chain from a push to a queued evaluation.

-   :material-harddisk: **[NAR Storage](nar-storage.md)**

    Object layout, idempotent writes, bucket requirements, closure rows and the deep GC.

-   :material-cloud-upload: **[Cache Serving](cache-serving.md)**

    Signing, narinfo, pull-through, debug info and the status codes under `/cache/`.

-   :material-graph-outline: **[Graph Queries](graph-queries.md)**

    Recursive walks, the `OFFSET 0` fence, indexes, counter ripples, metrics and the graph API.

-   :material-key: **[Authentication](authentication.md)**

    Sessions, API keys, download tokens and OIDC.

</div>

## Covered Elsewhere

| Topic | Page |
|---|---|
| Evaluation steps, fetch and walk | [Jobs](../proto/jobs.md), [Eval Worker Setup](../eval-worker.md) |
| Batch ingest, anchors | [Build Anchors](../scheduler/build-anchors.md) |
| Promotion, dispatch gates, failure cascade | [Promotion and Counters](../scheduler/promotion-and-counters.md) |
| Offers, assignment, scoring | [Capabilities and Dispatch](../proto/capabilities-and-dispatch.md), [Scoring](../scheduler/scoring.md) |
| Uploads and downloads | [Transfer](../proto/transfer.md) |
| Wholeness, cache access | [Cache Closure](../scheduler/cache-closure.md) |
| Worker registration and auth | [Connection](../proto/connection.md) |
| Statuses | [Evaluations and Builds](../../concepts/evaluations-and-builds.md) |
