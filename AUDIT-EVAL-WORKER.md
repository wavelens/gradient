# AUDIT-EVAL-WORKER.md - Eval Worker & Resolver

## Scope

The Nix evaluation subsystem: how a flake is evaluated in an isolated
subprocess, how attributes are resolved, and how the eval-worker pool is
managed. All file:line references are against `main` at audit time.

Files in scope:
- `backend/gradient-eval/src/` (~1.7k LOC): `eval_worker.rs` (577),
  `wildcard_walk.rs` (667), `flake_walk.rs` (189), `nix_eval.rs` (102),
  `jobs.rs` (108), `stats.rs` (51), `lib.rs` (29).
- `backend/gradient-worker/src/worker_pool/`: `pool.rs` (894),
  `resolver.rs` (680), `eval_stats.rs` (140), `mod.rs` (12).
- `backend/gradient-worker/src/nix/`: `store.rs` (430), `gcroots.rs` (228),
  `log.rs` (65), `mod.rs` (16).
- `backend/gradient-worker/src/executor/eval.rs` (1263), the executor angle
  (also referenced by AUDIT-PROTOCOL.md).

### Accuracy note (correction to the planning stub)

Two things the planning notes assumed are NOT on `main`:

- The eval-worker IPC is **line-delimited JSON**, not rkyv. Parent side:
  `EvalWorker::request` does `serde_json::to_vec(req) + b'\n'` then
  `read_line` (`pool.rs:167-231`). Subprocess side: `run_eval_worker` reads
  JSON lines and writes them back (`eval_worker.rs:109-378`). The rkyv +
  streaming rework (PR #465) has not landed.
- `Resolve` is **batched, not streaming**. A whole batch of attrs goes in one
  `EvalRequest::Resolve` and comes back as one `EvalResponse::ResolveOk {
  items: Vec<ResolvedItem>, .. }` (`eval_worker.rs:257-317`, `pool.rs:332`).
  There is no `ResolveItem`/`ResolveEnd` per-attr streaming on `main`;
  streaming exists only as the unmerged PR. This audit describes the batched
  reality.

## Eval worker - current flow

Flake reference to `.drv` closure push. Two processes are involved: the
async worker (parent, Tokio) and one or more single-threaded eval-worker
subprocesses (Boehm-GC libnix, no Tokio).

```
  PARENT (async worker, Tokio)                    SUBPROCESS (--eval-worker, sync libnix)
  ============================                    =======================================

  executor/eval.rs
    evaluate_derivations()                (eval.rs:199)
      |  build_flake_url()                (eval.rs:333)  path:/nix/store | git+file://?rev=
      |  fingerprint + pull eval-cache blob (best-effort, eval.rs:209-241)
      v
    evaluate_derivations_with()           (eval.rs:723)   testable core over trait objects
      |
      | Step 1: list ---------------------------------> WorkerPoolResolver
      |   resolver.list_flake_derivations() (resolver.rs:337)
      |     pool.acquire() --------------------------->  EvalRequest::Plan   --+
      |       plan on ONE worker (cheap prefix force)                          |
      |     <----------------------------------------   EvalResponse::PlanOk   |
      |     fan shards across pool (resolver.rs:391)                           v
      |       per shard: EvalRequest::List ---------->   ev.walker(repo)   FlakeWalker::open
      |                                                    lock_flake + EvalCache::open (flake_walk.rs:36)
      |                                                    wildcard_walk::discover_patterns
      |                                                      walk() over CursorNode (wildcard_walk.rs:75)
      |     <----------------------------------------   EvalResponse::ListOk { attrs, warnings, stats }
      |
      | Step 2: resolve ------------------------------> EvalRequest::Resolve (batched, <=64 attrs)
      |   resolver.resolve_derivation_paths() (resolver.rs:425)
      |     dynamic work-queue of small batches                walker.resolve(attr) -> drv_path
      |     resolve_chunk() crash-isolation bisect (resolver.rs:119)   (flake_walk.rs:85)
      |     <----------------------------------------   EvalResponse::ResolveOk { items[], stats }
      |
      | Step 3-5: BFS closure walk (ClosureWalker, eval.rs:457)
      |   process_wave(): read+parse .drv from local store, DRV_READ_CONCURRENCY=64
      |     query_known_derivations() prune known subtrees (eval.rs:549)
      |     every EVAL_BATCH_SIZE=50:
      |       updater.push_drv_closure(produced_drvs)   (push sources BEFORE report)
      |       mark_substituted() via query_cache
      |       updater.report_eval_result(batch)  --> server promotes+dispatches mid-eval
      v
    take_eval_stats() -> report_eval_stats           (eval.rs:255)
    checkpoint_cache() ----------------------------> EvalRequest::Checkpoint  (fold WAL -> .sqlite)
    read blob + push_eval_cache()                    (eval.rs:274-279)
```

### Eval-pool lifecycle

```
  EvalWorkerPool  (pool.rs:484)
    idle: Vec<EvalWorker>   (LIFO stack, Mutex)
    semaphore: max permits = pool size
    live: HashSet<pid>      (reaper target registry)

  acquire()  (pool.rs:546)
    1. semaphore.acquire_owned()               back-pressure on pool size
    2. while under_pressure && not-the-only-eval: sleep 200ms   (pool.rs:556)
    3. test-on-borrow: pop idle; if !is_alive() (try_wait, pool.rs:160) drop the
       corpse and loop; else reuse. Idle vec empty -> EvalWorker::spawn().
         spawn() (pool.rs:69): re-exec current_exe --eval-worker,
           env NIX_CACHE_HOME=eval_cache_dir, pre_exec setrlimit STACK=64 MiB,
           /proc/<pid>/oom_score_adj = "600"  (kernel sacrifices evals first),
           register pid in live set (PidGuard deregisters on drop).

  PooledEvalWorker::drop  (pool.rs:681)
    shutting_down ? graceful worker.shutdown() (atexit: flush eval-cache SQLite)
    recycle_after>0 && overused ? graceful shutdown          [DEAD: recycle_after==0]
    healthy ? push back to idle
    else drop (kill_on_drop SIGKILLs child)
    (per-call resolver.rs also mark_dead + discard when rss_bytes > max_eval_rss)

  memory_reaper_loop  (pool.rs:434)   spawned iff min_free_bytes>0
    every 500ms: if MemAvailable < margin -> SIGKILL largest live eval pid,
    latch under_pressure so acquire() throttles. Turns a would-be host OOM into
    one bounded eval failure (parent sees the pipe close).

  shutdown()  (pool.rs:606)  set shutting_down BEFORE close semaphore; drain
    idle and send Shutdown concurrently, 5s per worker. Idempotent.
```

Pool size = `budgeted_pool_size(fork_workers, max_eval_rss, ram_budget)`
(`pool.rs:392`), floored at 1, capped so `size * max_eval_rss <= 0.75 * RAM`
(`EVAL_RAM_SHARE`, `eval.rs:68`). The subprocess hosts one persistent
`NixEvaluator` (one `EvalState`, eval-cache + pure-eval on, `nix_eval.rs:42`);
Boehm GC needs stop-the-world signals that Tokio threads block, which is the
whole reason evaluation is exiled to a synchronous subprocess.

## Messiness & code smells

Ranked by impact against the "one legible flow" goal.

**1. `pool.rs` (894) is a four-concern god-file.** One file carries: the
subprocess handle + JSON IPC transport (`EvalWorker`, `pool.rs:28-377`), the
pool + RAII checkout lifecycle (`EvalWorkerPool`/`PooledEvalWorker`,
`:484-736`), the memory-budget math (`budgeted_pool_size`,
`memory_guard_bytes`, `rss_of_pid`, `:392-424`), and the background reaper
(`memory_reaper_loop`, `:434-477`). "How a worker is spawned, checked out,
recycled, reaped" is one story, but the wire framing and the RAM arithmetic
are braided through it.

**2. Eval-worker IPC is a second wire format (JSON lines) alongside the
system's rkyv.** `EvalWorker::request` (`pool.rs:167-231`) hand-rolls
`serde_json::to_vec + '\n'` / `read_line`; the subprocess mirrors it with its
own `write_response` (`eval_worker.rs:373`). This is a whole parallel
`EvalRequest`/`EvalResponse` protocol (`eval_worker.rs:26-99`) with its own
malformed-frame handling, disjoint from `gradient-proto`. (AUDIT-PROTOCOL.md
smell 7.)

**3. Response-shape checking is copy-pasted on both sides.** Parent: five
methods each end with `_ => anyhow::bail!("unexpected response to X")`
(`pool.rs:248,271,283,295,348`), each also bumping the dead
`evaluations_served` counter. Subprocess: the giant `run_eval_worker` match
(`eval_worker.rs:192-354`) repeats the `let Some(ev) = evaluator.as_ref()
else { write Err; continue }` guard five times
(`:201,229,258,319,336`) and re-derives `ev.walker(&repository)` per arm
(`:213,240,270,346`) - no walker reuse across a Plan/List/Resolve sequence
for the same repo.

**4. `resolver.rs` (680): duplicated fan-out and split-brain crash recovery.**
`list_once` and `resolve_once` (`:269-332`) are the same seven-line ritual
(acquire, call, observe_stats, rss check + conditional mark_dead, or
mark_dead + Err). The dynamic work-queue block (FuturesUnordered over
`Mutex<VecDeque>` with the scope-the-pop-before-await dance) is written twice,
once for shards (`:391-413`) and once for resolve batches (`:476-497`). Worse,
the two crash-recovery strategies diverge: listing does a flat retry-once
(`list_shard`, `:254-263`) while resolving does `O(log n)` bisection
(`resolve_chunk`, `:119-166`) for the identical failure (a subprocess crash).

**5. `wildcard_walk.rs` (667): `walk` and `plan_one` are structural twins that
must be kept in lockstep by hand.** `walk` (`:75-147`) emits matched
derivations; `plan_one` (`:211-272`) emits shards; both re-implement the exact
`*` / `#` / opaque / recover-one-level branch semantics. The existence of the
`assert_split_equivalent` test (`:544`) is the tell: they can silently drift,
so a test polices an invariant that a single traversal would make
structural.

**6. Recycle-by-count machinery is vestigial (dead code).** `recycle_after` is
hardcoded to `0` at the only construction site (`pool.rs:590`), so the entire
`overused` branch in `PooledEvalWorker::drop` (`:702-722`) never fires and the
`evaluations_served` field plus its five increments (`:238,257,276,288,337`)
feed nothing. RSS-based reclamation (`resolver.rs:285,321`) fully replaced it;
the count path was left wired but off.

**7. `PooledEvalWorker::drop` repeats the graceful-shutdown branch three
times** (`pool.rs:681-736`): the shutting-down branch, the recycle branch, and
the healthy-return branch each re-derive the "if healthy and a runtime handle
exists, spawn `worker.shutdown()`, else drop" decision. (AUDIT-PROTOCOL.md
smell 8.)

**8. Blocking `std::fs` inside async fns.** `EvalWorker::spawn` writes
`oom_score_adj` with `std::fs::write` (`pool.rs:120`); `request`'s dead-pipe
diagnostics read `/proc/<pid>/{fd/1,status,wchan}` synchronously
(`pool.rs:199-209`); `rss_bytes`/`rss_of_pid` read `/proc/<pid>/statm`
synchronously (`pool.rs:361,413`), the latter from the reaper's async loop.
None use `spawn_blocking`. (AUDIT-PROTOCOL.md smell 6.) By contrast
`resolver.rs:510` correctly uses `tokio::fs::read` for `.drv` files.

**9. Store-path prefix helpers are re-implemented 4+ times.**
`gradient-eval/src/lib.rs` defines `nix_store_path` + `strip_nix_store_prefix`
(`:18-29`); `resolver.rs:21` re-defines `nix_store_path` with a "mirrors the
helper" comment; `nix/store.rs` has `strip_store_prefix` +
`canonicalize_store_path` (`:301-319`); `gcroots.rs:93` inlines
`strip_prefix("/nix/store/")`. The same trivial concept, four call sites, no
`StorePath` type doing it once.

**10. Magic constants split between named consts and inline literals.** Named
but scattered: `MAX_CRASH_ATTEMPTS=2`/`MAX_RESOLVE_BATCH=64`
(`resolver.rs:106,111`), `DRV_READ_CONCURRENCY=64`/`EVAL_RAM_SHARE=0.75`/
`EVAL_BATCH_SIZE=50` (`eval.rs:64,68,185`). Inline and unnamed: the
`oom_score_adj` value `"600"` (`pool.rs:120`), stack `64 * 1024 * 1024`
(`pool.rs:95`), reaper `500ms` (`pool.rs:443`), back-pressure `200ms`
(`pool.rs:560`), shutdown `5s`/`2s` timeouts (`pool.rs:322,191`). The 4 GiB
memory fallback (`eval.rs:78`) is another bare literal.

**11. Merged/misplaced doc comment in `nix/store.rs:182-209`.** The BFS
`collect_runtime_closure` docstring (`:182-189`) runs straight into
`add_indirect_root`'s docstring mid-paragraph, so the closure-walk prose is
attached to the wrong function and `collect_runtime_closure` (`:211`) is left
effectively undocumented. A concatenation bug from an edit.

**12. Two "stats" modules with a re-export shuffle.** The wire delta lives in
`gradient-eval/src/stats.rs` and the accumulator in
`worker_pool/eval_stats.rs`, which re-exports `StatsDelta`/`metrics_enabled`
from the former (`eval_stats.rs:10`). Reasonable layering, but both are named
"stats", and `EvalStatsTotals`/`EvalAttrCost` are then re-shaped again into
`EvalStatsReport` in `eval.rs:682`, so a delta crosses three type
representations before it reaches the wire.

## Refactoring recommendations

Concrete and ranked. Each collapses a split flow into one place, per AUDIT.md.

**1. Split `pool.rs` along its four seams.** `eval_worker/transport.rs`
(the `EvalWorker` handle + framing), `eval_worker/pool.rs`
(`EvalWorkerPool` + `PooledEvalWorker` + `PidGuard`), and
`eval_worker/memory.rs` (`budgeted_pool_size`, `memory_guard_bytes`,
`rss_of_pid`, `memory_reaper_loop`). The pool file then reads as one lifecycle
story with the RAM math and wire bytes behind named helpers.

**2. Put the eval-worker IPC on the shared rkyv framing and one generic
matcher.** Replace `request()`'s JSON with the `u32 len + rkyv bytes` frame
the main protocol already uses, and collapse the five per-method
`_ => bail!("unexpected response to X")` into a single
`request::<Resp>()` / `expect_response::<T>()`. On the subprocess side, wrap
the per-arm guard in one `with_evaluator(repo, |walker| ...)` helper that
opens the walker once and returns the typed `Err` when the evaluator failed to
init. Add a one-byte version handshake. Aligns with AUDIT-PROTOCOL.md rec 7.

**3. Unify the pooled fan-out and the crash-isolation policy.** Extract one
generic `pooled_fan_out<In, Out>(pool, work_items, |worker, item| ...)` that
both discovery (shards) and resolution (batches) drive, and route both through
the pure `resolve_chunk` bisection so listing and resolving recover from a
subprocess crash the same way. Fold `list_once`/`resolve_once` into that one
call site (acquire, observe_stats, rss-recycle, mark_dead-on-Err).

**4. Merge `walk` and `plan_one` into one visitor-parameterized traversal.**
A single recursive `traverse(node, path, segs, &mut visitor)` where the
visitor is either "emit matched derivation" or "emit shard at first
wildcard". The `*`/`#`/opaque semantics then live once; delete
`assert_split_equivalent` as a structural guarantee (keep one behavioural
test).

**5. Collapse `PooledEvalWorker::drop` to one decision + one action.** Compute
a `Disposition` (`GracefulShutdown | ReturnToIdle | Kill`) once from
`shutting_down` / `overused` / `healthy`, then act once. Removes the triplicated
"spawn shutdown if runtime else drop" block.

**6. Delete the dead recycle-by-count path, or wire it to config.** Drop
`evaluations_served`, `recycle_after`, and the `overused` Drop branch (RSS
recycling already covers the intent), or thread a real
`GRADIENT_EVAL_RECYCLE_AFTER` through if count-based recycling is still wanted.
Leaving it half-wired is a trap for the next reader.

**7. One store-path type/helper.** Consolidate on
`gradient-eval::{nix_store_path, strip_nix_store_prefix}` (or the existing
`StorePath` value type) and delete the re-definitions in `resolver.rs`,
`nix/store.rs`, and the inline strip in `gcroots.rs`.

**8. Move the `/proc` and `oom_score_adj` IO off the async path and name the
constants.** Wrap the `std::fs` writes/reads in `spawn_blocking` (or accept and
document them as cheap, sub-page reads), and lift `"600"`, the 64 MiB stack,
the reaper/back-pressure intervals, and the shutdown timeouts into named
`const`s next to their peers.

**9. Fix the merged docstring in `nix/store.rs:182-209`** so each function
carries its own doc.

## Related

- **AUDIT-PROTOCOL.md** covers the eval-worker subprocess boundary from the
  protocol side: the JSON-line IPC (smell 7 / rec 7), the triplicated
  `PooledEvalWorker::drop` (smell 8), and the blocking `std::fs` calls
  (smell 6). This file goes deeper on evaluation semantics, the resolver
  fan-out, wildcard traversal, and pool/memory lifecycle.
- **AUDIT.md** is the index and states the "one legible flow" north star that
  the recommendations above target.
