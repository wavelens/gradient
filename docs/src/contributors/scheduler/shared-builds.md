# Shared Builds

A derivation is built once, globally. The build state of the derivation is on one `derivation_build` row, the *shared build*. All evaluations of that derivation point at this row. Each write to shared builds and the surrounding graph must go through one graph writer (`backend/gradient-graph`).

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
| `derivation` | UNIQUE `(hash, name)` | The global graph node. `walked` and `unwalked_inputs` record whether the subtree is known |
| `derivation_build` | UNIQUE `derivation` | The shared build: `status`, `attempt`, `cache_available`, `probed`, `fetchable`, `wanted`, `blocking_deps`, `missing_runtime_deps`, limits |
| `build_job` | `evaluation`, `derivation_build` | Link from one evaluation to a shared build, carrying the assignment `score` |
| `build_attempt` | `derivation_build`, `dispatched_job` | One execution: `outcome`, `reason`, timings, `build_context` |
| `build_log_chunk` | `build_attempt` | The attempt's log |

| Foreign key | On delete | Effect |
|---|---|---|
| `derivation_build.derivation` | `CASCADE` | The shared build will live as long as the derivation |
| `build_job.evaluation` | `CASCADE` | Evaluation GC will drop its links |
| `build_attempt.build_job` | `SET NULL` | An attempt and its log outlive the driving evaluation |
| `build_attempt.derivation_build` | `CASCADE` | Attempts go with the shared build |

## Sharing

- Two evaluations (same or different projects) needing one derivation share its build. There is no leader, follower or link row. Sharing is the global graph itself.
- `BatchWriter::resolve_shared_builds` (`record.rs`) can insert new shared builds with `ON CONFLICT (derivation) DO NOTHING` semantics. A shared build from an earlier evaluation will stay untouched.
- The first assignment will build. All other evaluations see the result once the shared build is `Completed` or `Substituted` (`BuildStatus::TERMINAL_SUCCESS`).
- A `Completed` shared build reused later will keep its attempts and logs until garbage collection of the derivation.

## Graph Writer

The graph writer, `GraphWriter` (`gradient-graph/src/writer.rs`), is a root child of the supervision tree, stopped after every sibling. Callers use the `Graph` handle (`lib.rs`). One message is one transaction. Queued `Record` and `CommitNar` messages are the exception and share one.

| `GraphMsg` | Written |
|---|---|
| `Record` | One worker batch: derivations, stubs, edges, outputs, input sources, shared builds, `build_job` rows, features, messages, entry points, the can-start counters and the needs-build marks |
| `UpstreamHits` | Probe results: narinfo on `derivation_output`, the runtime dependencies named in the narinfo, `cache_available`, the needs-build marks moved |
| `UpstreamProbed` | `probed = true` on answered shared builds, and the needs-build marks set below them by a miss |
| `CommitNar` | The `cached_path` row, its references, signed `cached_path_signature` rows, the backed outputs |
| `Transition` | Evaluation and shared build state: stream completed, eval failed, build started/output/completed/failed, assigned, orphaned, can start, repair, abort, prioritize |
| `Requeue` | `FailedTransient` shared builds whose backoff elapsed, back to `Queued` |
| `Demote` | `MissingNar`, operator `Path` invalidation, one cache dropping its `CacheClaim` |
| `Gc` | Bounded deletes of derivations, stale paths and evaluations, re-checked against rows that became live since the scan |

All other messages flush the queue first. A write after a batch or a commit will see that batch or commit.

`Graph::known_derivations` is not a message. The call can read from the pool which `drv_hashes` hold a recorded subtree (`walked` and `unwalked_inputs = 0`). The next wave of a walk can then start before its previous batch is recorded. A recorded subtree can only ever gain its record. A read missing a queued write will skip less, never wrongly.

## Batching

- Queued batches to record share one transaction with a savepoint each (`record_one`). A batch failing for its content will fail only its caller.
- The graph writer can execute the queued `CommitNar` messages set-based under one savepoint (`nar::commit_batch`). Each step is one statement for the whole batch instead of a dozen per NAR. The cost of a commit is round trips. A failed batch will commit its NARs one by one (`commit_one`), each under its own savepoint. A bad NAR will fail only its uploader.
- A flush can take place on the next mailbox turn. The flush is immediate once the queue is at `RECORD_ROW_BUDGET` (5000 derivations) or `NAR_COMMIT_BUDGET` (128 commits). After `NAR_FLUSH_TIME` (100 ms) a flush will take no more NAR commits and leave the rest to the next mailbox turn. The flush must hold the shared build locks of every commit until its end.
- The wire of the worker is without an acknowledgement to retry on. A failed batch will fail its evaluation (`fail_evaluation`) instead of leaving the graph incomplete.
- `RecordBatch.truly_substituted` is the one fact from outside the graph. The field can list per drv path the derivations with a complete closure already in our cache, as established in the scheduler. Their new shared builds start `Substituted`. The transaction must assign the ids itself.
- Upstream availability is not part of a batch. The probe can answer through `UpstreamHits` for shared builds needing a build. A batch can write `cache_available = false` on new shared builds only.
- Post-commit (`record::after_commit`): entry point statuses, per-task evaluation GC, probe requests, the `evaluation::Progress` event.

## Timeouts and Retries

| Constant | Value | Meaning |
|---|---|---|
| `CALL_TIMEOUT` | 30 s | Wait for the graph writer to exist after a restart |
| `GRAPH_TX_BUDGET` | 120 s | Per transaction. The transaction will roll back past this budget, and the caller will get `graph transaction exceeded 120s` as error. Also set as the transaction's `statement_timeout`. The rollback must wait for the running statement, and only Postgres can end that statement |
| `GRAPH_TX_ATTEMPTS` | 3 | Attempts of a transaction aborted with SQLSTATE `40P01`, `40001` or `25P02` |

- A caller will wait for its reply without a deadline. The graph writer may still apply a message whose caller gave up. A caller-side timeout would report a still-landing write as failed. A worker session behind a backlog will block its reader as backpressure.
- A deadlock inside one batch will fail the whole flush (`escalate_retryable`), and `transact` will retry the flush.
- Status updates and their effects return every database error to the `transact` function. A `SELECT 1` probe before `COMMIT` can turn a transaction aborted with a merely logged error into a `25P02` error. Postgres would otherwise answer that `COMMIT` with a silent rollback.
- The retry may re-send board events and probe requests already sent in an aborted attempt.
- `Transition::Repair` (`gradient_db::graph::repair::repair_build_graph`) and cache demotion take place inside the graph writer. The consistency check (`consistency_check_pass`, `gradient-scheduler`) is outside, without retries. Its next pass will pick the work up.
- The graph writer can hold no state between messages. A restart will lose only batches still queued, and their callers get an error.

## Concurrent Walks

- Two walks of overlapping graphs converge on one `derivation` row per `(hash, name)` pair. `WALKED_UPSERT` and `STUB_INSERT` use `ON CONFLICT (hash, name)`.
- Edge inserts (`derivation_dependency`) use `ON CONFLICT DO NOTHING`. Two walks of one node record the same set.
- `accepts_batches` can take batches only while the evaluation is `Fetching`, `EvaluatingFlake` or `EvaluatingDerivation`. Other batches get dropped as stale (`RecordReport.skipped`).
- Two walks of one evaluation differ in the assignment id minted at hand-out. The worker must echo the id on every report. The session will drop a report for an assignment that another session handed out (`gradient-proto/src/handler/inbound.rs`, `owned`).

## Cache Availability Flag

`cache_available` can mark a shared build with outputs available in an upstream cache.

- Set only by the `FLIP_CACHE_AVAILABLE` statement (`UpstreamHits`, `record.rs`), only on shared builds not in `TERMINAL_SUCCESS`.
- One project's probe can add a substitution for another project's walk.
- Cleared by `exhaust_substitution` (`transition.rs`) once substitute misses escalate. The shared build will return to `Created` as an ordinary build.
- Cleared by `CLEAR_CACHE_AVAILABLE_TRUST` (`gradient-db/src/caches/demotion.rs`) for an output proven unfetchable.

## Counter Locking

Counters stay correct without the graph writer's serialisation. A second writer can share them (`gradient-db/src/graph/shared_build_guard.rs`).

- Advisory keys: namespace `SHARED_BUILD_LOCK_NAMESPACE` (643), key `hashtext(derivation id)`, taken sorted in one pass before any row lock.
- A seed of `blocking_deps` or `missing_runtime_deps` must hold the keys of its dependencies in shared mode.
- A change of the complete-closure state or of `fetchable` must hold the key of the shared build exclusively. The hold is in place before the spread can read the edges into the shared build.
- The second of a seed and a concurrent state change will wait for the first to commit, then read its rows.
- Shared keys never wait on each other. A dependency referenced by every `.drv` (`source-stdenv.sh`) will cause no queueing.
- `wanted` is the exception. `update_need` can lock only its roots. The `recount_wanted` step of the consistency check can correct drift. A lost update can leave a shared build `Skipped` until the next check.

## Related

- [Jobs](../proto/jobs.md) - build job reports that drive `Transition`
- [Architecture](../architecture.md)
