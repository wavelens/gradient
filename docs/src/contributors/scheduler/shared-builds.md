# Shared Builds

A derivation is built once, globally. Its build state lives on one `derivation_build` row, the *shared build* every evaluation of that derivation points at. Every write to shared builds and the graph around them goes through one graph writer (`backend/gradient-graph`).

```mermaid
flowchart LR
    E1[evaluation A] --> J1[build_job]
    E2[evaluation B] --> J2[build_job]
    J1 --> A[derivation_build]
    J2 --> A
    A --> D[derivation]
    A --> T[build_attempt]
    T --> L[build_log_chunk]
```

## Tables

| Table | Key | Role |
|---|---|---|
| `derivation` | UNIQUE `(hash, name)` | The global graph node; `walked` and `unwalked_inputs` record whether the subtree is known |
| `derivation_build` | UNIQUE `derivation` | The shared build: `status`, `attempt`, `cache_available`, `probed`, `fetchable`, `wanted`, `blocking_deps`, `missing_runtime_deps`, limits |
| `build_job` | `evaluation`, `derivation_build` | Links one evaluation to a shared build; carries the assignment `score` |
| `build_attempt` | `derivation_build`, `dispatched_job` | One execution: `outcome`, `reason`, timings, `build_context` |
| `build_log_chunk` | `build_attempt` | The attempt's log |

| Foreign key | On delete | Effect |
|---|---|---|
| `derivation_build.derivation` | `CASCADE` | The shared build lives as long as the derivation |
| `build_job.evaluation` | `CASCADE` | Evaluation GC drops its links |
| `build_attempt.build_job` | `SET NULL` | An attempt and its log outlive the driving evaluation |
| `build_attempt.derivation_build` | `CASCADE` | Attempts go with the shared build |

## Sharing

- Two evaluations (same or different projects) needing one derivation share its build. No leader, follower or link row exists; sharing is the global graph itself.
- New shared builds are inserted `ON CONFLICT (derivation) DO NOTHING`: one from an earlier evaluation stays untouched (`BatchWriter::resolve_shared_builds`, `record.rs`).
- The first assignment builds; every other evaluation sees the result once the shared build reaches `Completed` or `Substituted` (`BuildStatus::TERMINAL_SUCCESS`).
- A `Completed` shared build reused later keeps its attempts and logs until the derivation is garbage collected.

## Graph Writer

The graph writer, `GraphWriter` (`gradient-graph/src/writer.rs`), is a root child of the supervision tree, stopped after every sibling. Callers use the `Graph` handle (`lib.rs`); one message is one transaction, except that queued `Record` and `CommitNar` messages share one.

| `GraphMsg` | Writes |
|---|---|
| `Record` | One worker batch: derivations, stubs, edges, outputs, input sources, shared builds, `build_job` rows, features, messages, entry points, the can-start counters and the needs-build marks |
| `UpstreamHits` | Probe results: narinfo on `derivation_output`, the runtime dependencies the narinfo names, `cache_available`, the needs-build marks moved |
| `UpstreamProbed` | `probed = true` on answered shared builds, and the needs-build marks a miss sets below them |
| `CommitNar` | The `cached_path` row, its references, signed `cached_path_signature` rows, the backed outputs |
| `Transition` | Evaluation and shared build state: stream completed, eval failed, build started/output/completed/failed, assigned, orphaned, can start, repair, abort, prioritize |
| `Requeue` | `FailedTransient` shared builds whose backoff elapsed, back to `Queued` |
| `Demote` | `MissingNar`, operator `Path` invalidation, one cache dropping its `CacheClaim` |
| `Gc` | Bounded deletes of derivations, stale paths and evaluations, re-checked against rows that became live since the scan |

Every other message flushes the queue first: a write after a batch or a commit sees that batch or commit.

`Graph::known_derivations` is not a message: the call reads which `drv_hashes` have a recorded subtree (`walked` and `unwalked_inputs = 0`) from the pool. A walk's next wave then starts without waiting until its previous batch is recorded. A recorded subtree only gains its record: a read that misses a queued write prunes less, never wrongly.

## Batching

- Queued batches to record share one transaction with a savepoint each (`record_one`); one that fails for its content fails only its caller.
- Queued NAR commits run set-based under one savepoint (`nar::commit_batch`): one statement per step for the whole batch instead of a dozen per NAR, since a commit's cost is round trips. If the batch fails, its NARs commit one by one (`commit_one`), each under its own savepoint; a bad one fails only its uploader.
- A flush takes place on the next mailbox turn, or at once when the queue reaches `RECORD_ROW_BUDGET` (5000 derivations) or `NAR_COMMIT_BUDGET` (128 commits). A flush stops taking NAR commits after `NAR_FLUSH_TIME` (100 ms) and leaves the rest to the next mailbox turn, since the flush holds the shared build locks of every commit to its end.
- The worker's wire has no acknowledgement to retry on. A failed batch fails its evaluation (`fail_evaluation`) instead of leaving the graph incomplete.
- `RecordBatch.truly_substituted` is the one fact from outside the graph: derivations whose closure is already complete in our cache, established by the scheduler, per drv path. Their new shared builds start `Substituted`. Ids are assigned inside the transaction.
- Upstream availability is not part of a batch; the probe answers through `UpstreamHits` for shared builds that need building. A batch writes `cache_available = false` on new shared builds only.
- Post-commit (`record::after_commit`): entry point statuses, per-task evaluation GC, probe requests, the `evaluation::Progress` event.

## Timeouts and Retries

| Constant | Value | Meaning |
|---|---|---|
| `CALL_TIMEOUT` | 30 s | Wait for the graph writer to exist after a restart |
| `GRAPH_TX_BUDGET` | 120 s | Per transaction; past this the transaction rolls back and the caller gets `graph transaction exceeded 120s`. Also set as the transaction's `statement_timeout`: the rollback waits for the running statement, and only Postgres can end that statement |
| `GRAPH_TX_ATTEMPTS` | 3 | Attempts of a transaction aborted with SQLSTATE `40P01` or `40001` |

- A caller waits for its reply without a deadline. The graph writer still applies a message whose caller gave up; a caller-side timeout would report a write that still lands as failed. A worker session behind a backlog blocks its reader as backpressure.
- A deadlock inside one batch fails the whole flush (`escalate_retryable`), and `transact` retries the flush.
- Board events and probe requests an aborted attempt already sent are sent again by the retry.
- `Transition::Repair` (`gradient_db::repair_build_graph`) and cache demotion take place inside the graph writer. The consistency check (`consistency_check_pass`, `gradient-scheduler`) is outside and is not retried; its next pass picks the work up.
- No state is held between messages: a restart loses only batches still queued, and their callers get an error.

## Concurrent Walks

- Two walks of overlapping graphs converge on one `derivation` row per `(hash, name)`: `WALKED_UPSERT` and `STUB_INSERT` use `ON CONFLICT (hash, name)`.
- Edges (`derivation_dependency`) are inserted `ON CONFLICT DO NOTHING`; two walks of one node record the same set.
- `accepts_batches` takes batches only while the evaluation is `Fetching`, `EvaluatingFlake` or `EvaluatingDerivation`. Other batches are dropped as stale (`RecordReport.skipped`).
- Two walks of one evaluation differ by the assignment id minted when the job is handed out. The worker echoes the id on every report; a report for an assignment the session did not hand out is dropped (`gradient-proto/src/handler/inbound.rs`, `owned`).

## Cache Availability Flag

`cache_available` marks a shared build whose outputs are available in an upstream cache.

- Set only by the `FLIP_CACHE_AVAILABLE` statement (`UpstreamHits`, `record.rs`), only on shared builds not in `TERMINAL_SUCCESS`.
- One project's probe can add a substitution for another project's walk.
- Cleared by `exhaust_substitution` (`transition.rs`) once substitute misses escalate: the shared build returns to `Created` as an ordinary build.
- Cleared by `CLEAR_CACHE_AVAILABLE_TRUST` (`gradient-db/src/cache_storage.rs`) for an output proven unfetchable.

## Counter Locking

Counters stay correct without the graph writer's serialisation; a second writer can share them (`gradient-db/src/shared_build_guard.rs`).

- Advisory keys: namespace `SHARED_BUILD_LOCK_NAMESPACE` (643), key `hashtext(derivation id)`, taken sorted in one pass before any row lock.
- A seed of `blocking_deps` or `missing_runtime_deps` holds the keys of its dependencies shared.
- A flip of the complete-closure state or `fetchable` holds the shared build's key exclusively before the ripple reads the edges into the shared build.
- The second of a seed and a concurrent flip waits for the first to commit, then reads its rows.
- Shared keys never wait on each other: a dependency every `.drv` references (`source-stdenv.sh`) queues nothing.
- `wanted` is the exception: `update_need` locks only its roots. The consistency check's `recount_wanted` corrects drift; a lost update leaves a shared build `Skipped` until the next check.

## Related

- [Jobs](../proto/jobs.md) - build job reports that drive `Transition`
- [Architecture](../architecture.md)
