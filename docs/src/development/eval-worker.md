# Eval worker setup

Gradient evaluates flakes with a pool of `--eval-worker` subprocesses that drive the embedded Nix C API. A single evaluation is split into one shard per system and fanned across the pool, with the pool sized to fit the host's memory. Results are written to a persistent, fleet-shared eval-cache, so a repeat evaluation of the same locked flake is mostly cache hits.

## Compared to nix-eval-jobs

| Dimension | Gradient | nix-eval-jobs |
|---|---|---|
| Parallelism model | Spawn pool of long-lived eval-workers; one eval is sharded by system and fanned across the pool | Fork short-lived children from a warm parent |
| Cross-run warmth | Persistent on-disk eval-cache keyed by flake fingerprint, so a repeat eval of the same locked flake is mostly cache hits (skips forcing and daemon round-trips) | None; cold every run, since copy-on-write warmth lives only within one run |
| Cross-machine cache | Fleet-shared eval-cache (pull and push of `<fp>.sqlite`), so a warm cache propagates across the worker fleet | None |
| Concurrent shared cache | Concurrent shards write one eval-cache without deadlock (WAL-append commits plus a single end-of-eval checkpoint) | Not applicable, as there is no shared cache |
| Memory safety | Automatic pool sizing so `pool_size * maxEvalRss` stays within a host-RAM share; a many-system flake completes even on a small host (degrading to one shard) and never OOMs | Manual `--workers` and `--max-memory-size` |
| Pipeline integration | Native: discovery feeds DB rows and build dispatch starts mid-eval (incremental flush); the closure walk prunes server-known derivations and marks cache-status substitution | Emits a JSON job stream that the consumer (Hydra and similar) integrates |
| Per-attribute failure isolation | A bad attribute becomes a per-attribute error and the eval continues; a crash triggers chunk bisection that isolates the crasher | Per-job error reporting via the fork boundary |
| Crash isolation | Subprocess boundary with retry and bisection | Fork boundary with re-fork |
| Cross-machine eval compute | Roadmap; today it is a single-host pool | Single-host |

Gradient's main advantage is treating the eval-cache as a first-class, persistent, fleet-shared artifact, so repeat and CI evaluations of the same locked flake are near-instant across the whole worker fleet, together with automatic memory-budgeted sizing that guarantees an evaluation completes instead of relying on manual worker and memory tuning.

## Memory safety

Two layers bound eval memory. **Pool sizing** (`maxEvalRss`, `GRADIENT_MAX_EVAL_RSS`, default 8 GiB) caps how many eval subprocesses run at once - `pool_size * maxEvalRss` stays within a host-RAM share - and recycles a worker whose RSS exceeds the cap *between* `list`/`resolve` calls. But that check is post-call: a single evaluation unit (a large aggregate, IFD chains, an accidental recursion blow-up) can balloon the Boehm-GC heap past the cap *within* one call and OOM the host before the recycle runs.

The **free-RAM reaper** is the proactive peak guard. A background loop samples host `MemAvailable` every 500 ms; when it falls below the safety margin (`minFreeRamMb`, `GRADIENT_MIN_FREE_RAM_MB`; `0` = adaptive `max(1 GiB, 10% of total RAM)`) it SIGKILLs the **largest live eval subprocess**. The victim's parent task sees its pipe close and reports the eval as failed - converting a would-be host OOM (which could kill the worker itself and, because the server only registers a *clean* disconnect, strand the job non-terminal) into a single bounded eval failure. Under sustained pressure `acquire` also back-pressures, serialising evaluations (always letting one proceed, so it can never deadlock). Eval subprocesses keep `oom_score_adj = 600` as a last-resort kernel fallback.
