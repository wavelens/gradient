# AUDIT-PROTOCOL.md - Worker Protocol, Executor & NAR Upload Path

Scope: the server-worker wire protocol (`backend/gradient-proto`), the worker-side executor (`backend/gradient-worker`), and the NAR upload/transfer path end to end (worker origin, transport, server ingest, object storage, `cached_path` rows). Produced by a multi-agent code audit. File:line references are against `main` at audit time.

Headline architectural finding: two protocol stacks coexist. Production runs a monolithic `handler/` stack; a cleaner, role-symmetric `session`/`server`/`cap` stack exists in-tree but is unfinished and unwired (`server/dispatch.rs` is an 8-line stub). And on the NAR path there are two divergent transports (presigned direct-PUT and server-relayed streaming) whose branch is re-expressed at least four times, with the S3 branch skipping server-side verification entirely.

---

# Part 1 - Worker protocol & executor

The server-worker link is a single rkyv-framed WebSocket. `ClientMessage` (worker to server) and `ServerMessage` (server to worker) are defined in `backend/gradient-proto/src/messages/{client,server}.rs` and encoded as length-prefixed binary frames by `session/frame.rs` (`ProtoSocket`/`ProtoReader`/`ProtoWriter`, `MAX_PROTO_MESSAGE_SIZE = 8 MiB`, `NAR_PUSH_CHUNK_SIZE = 4 MiB`). The transport is symmetric: the same `handler::handle_socket` drives both inbound (worker dials server, `proto_router` in `handler/mod.rs:44`) and outbound (server dials worker, `outbound.rs:31`) connections; only a `server_initiated` bool differs.

## Protocol & executor - current flow

A build job round-trip:

```
 SERVER (gradient-proto/src/handler)                WORKER (gradient-worker/src)
 -----------------------------------                ----------------------------
 session::run() select-loop                         worker/dispatch::run_dispatch_loop
   split -> ProtoReader + ProtoWriter                 conn.split() -> reader + ProtoWriter
   |                                                  |  heartbeat 10s -> RequestJob{kind}
   |  scheduler.request_job()                  <----- ClientMessage::RequestJob
   +- send_credentials_for_job (socket.rs:312)        |
   +- ServerMessage::AssignJob ---------------------> MessageHandler::dispatch (dispatch.rs:307)
                                                        +- on_assign_job -> spawn run_job()
                                              <-------- AssignJobResponse{accepted}
                                                        run_job -> execute_build_job (executor/mod.rs:370)
                                                          per BuildTask:
                                                            report_building -> JobUpdate{Building}
   RpcContext::on_cache_query (dispatch.rs:1116) <---- CacheQuery{Pull}   (prefetch_inputs)
   handle_cache_query (45s budget)                       |
   CacheStatus / CacheError ---------------------------> on_cache_status -> oneshot waiter (job.rs)
   serve_nar_request (socket.rs:72) - NarStreamHeader -> nar_recv.note_header
     stream from nar_storage - NarPush*/NarUnavailable > nar_recv.accept_chunk
                                                          build::build_derivation (build.rs:625)
                                                            ParsedDerivation::load -> .realize (daemon)
                                                            drain_build_logs -> LogChunk*
                                              <---------- JobUpdate{BuildOutput, metrics}
   handle_build_output                                    compress_and_push_paths:
   on_nar_uploaded (dispatch.rs:965)  <-- NarStreamHeader/NarPush* / presigned PUT
     put_nar_idempotent (ingest.rs:73)   - NarPushResume >
     mark_nar_stored                  <------ NarUploaded{file_hash,nar_hash,refs,deriver}
   handle_job_completed                <------ JobCompleted    (done_tx -> on_job_done, dispatch.rs:155)
        |  (or) handle_job_failed      <------ JobFailed{error, kind, missing_paths}
```

Routing tables that must stay hand-synchronized: `DispatchContext::dispatch` (`handler/dispatch.rs:270`), `MessageHandler::dispatch` (`worker/dispatch.rs:307`), plus the three parallel name maps `ClientMessage::variant_name` (`messages/client.rs:276`), `msg_kind` (`worker/dispatch.rs:757`), and the test-only `variant_of` (`handler/socket.rs:399`).

Failure classification flows worker to server: worker produces a `BuildError{kind, missing_paths}` (`build.rs:57`), `on_job_done` downcasts it into `JobFailed.kind: BuildFailureKind` (`worker/dispatch.rs:186`), and the scheduler consumes it in `decide_failure_outcome` (`gradient-scheduler/src/build.rs:47`).

Load-bearing architectural finding: there are two protocol stacks in `gradient-proto`. Production runs the monolithic `handler/` stack (`handler::handle_socket`). A newer, cleaner, role-symmetric stack (`session::handshake` pure FSM emitting `Intent`, `client::dial`, `server::{accept,dispatch}`, `cap::{build,eval,fetch,cache}` trait pairs, `traits`) exists but is not wired in: `server/dispatch.rs` is an 8-line stub ("Populated by Task 16.") and nothing outside the crate calls `as_peer`/`as_authority`. `lib.rs:26` states the `handler` module "Will shrink in follow-up refactors as the worker and gradient-server migrate to the new primitives." The refactor is half-done and stalled in-tree.

## Messiness & code smells

Ranked by impact.

**1. `handler/dispatch.rs` is a grab-bag, not a dispatcher (1467 lines).** Four unrelated concerns:
- The disk-backed inbound NAR staging store `NarReceiveStore` + `PathState` + `AppendOutcome` (`dispatch.rs:39-240`), ~200 lines of storage/poison/resume logic that belongs beside `serve_nar_request`.
- The routing match `DispatchContext::dispatch` (`dispatch.rs:270-478`), a 200-line match where arms are wildly inconsistent: some inline the whole handler (`WorkerMetrics` spawns, `CacheQuery`/`QueryKnownDerivations` spawn `RpcContext`), some call a private `on_*` method, some call free functions in `eval_cache`. No uniform handler contract.
- Business logic: `on_nar_uploaded` (`dispatch.rs:965-1056`) is a 90-line method doing integrity checks, staged-read, `put_nar_idempotent`, metric recording, and three abort/fail paths.
- The eval-BFS pruning policy `prunable_known_derivations` (`dispatch.rs:1249`), pure scheduler business logic parked in the transport handler.

**2. `executor/build.rs` mixes six responsibilities (1483 lines).** `build_derivation` (`:625`) is a reasonable orchestrator, but the file also carries: failure taxonomy (`BuildError` + 6 constructors, `:73-114`); OOM heuristics (`looks_like_oom`/`classify_build_error`, `:118-134`); cgroup/network metric sampling (`NetworkPeakSampler`, `CgroupSampler`, `newest_build_cgroup`, `merge_cgroup_sample`, `:243-358`); the daemon type-state pipeline (`ParsedDerivation`, `:406-547`); Hydra product parsing (`load_products`, `:590`); harmonia `BasicDerivation` construction (`get_basic_derivation`, `:943-1073`, 130 lines with a 3-way FOD/CA/input-addressed disambiguation); and the log-drain state machine (`drain_build_logs_with_timeout`, `:750-927`).

**3. Duplicated / fragmented error mapping across four sites.** Retry classification is smeared: inline `downcast_ref` chain mapping prefetch errors to `BuildError` in `execute_build_job` (`executor/mod.rs:456-468`); `classify_substitute_failure` (`executor/mod.rs:177`); `looks_like_oom`/`classify_build_error` string-sniffing (`build.rs:118-134`); the `BuildError` to `JobFailed.kind` downcast in `on_job_done` (`worker/dispatch.rs:186-192`, which silently defaults an un-downcastable error to `Permanent`). There is no single "anyhow::Error to BuildFailureKind" function; each site can drift. OOM detection by substring-matching build log text ("killed", "out of memory") is inherently fragile.

**4. `MessageHandler` has 18 borrowed fields (`worker/dispatch.rs:285-303`).** It is reconstructed field-by-field on every inbound message inside the select loop (`:107-125`), and `run_dispatch_loop`/`on_job_done` carry `#[allow(clippy::too_many_arguments)]` for the same reason (`:36,154`). The per-job waiter maps, abort senders, and capacity counters should be one owned struct.

**5. Two parallel handshake implementations.** `handler/session.rs` (`ProtoSession<Opening/Authenticated/Registered>` typestate + `decide_auth`) is live; `session/handshake.rs` (`Opening->Greeted->Authenticated->Registered` pure FSM + `Intent`) is the intended replacement, unused in production. Version negotiation (`if version != PROTO_VERSION`) is copy-pasted across `handler/session.rs:114`, `session/handshake.rs:75`, `cache_session.rs:68`, and `worker/connection/handshake.rs`. Dead scaffolding (`server/dispatch.rs` stub) ships in the binary.

**6. Blocking `std::fs` calls inside async fns.** `get_basic_derivation` reads every input `.drv` with synchronous `std::fs::read` (`build.rs:1011`) on the async build path; `CgroupSampler`'s spawned task calls sync `newest_build_cgroup`/`read_build_cgroup` on a 200 ms tokio timer (`build.rs:309-333`); `EvalWorker::spawn` writes `oom_score_adj` via `std::fs::write` in an async fn (`pool.rs:120`); `request`'s dead-pipe diagnostics read `/proc` synchronously (`pool.rs:199-209`). None are on `spawn_blocking`.

**7. Eval-worker IPC uses JSON lines while the rest of the system uses rkyv.** `EvalWorker::request` does `serde_json::to_vec(req) + '\n'` / `read_line` (`pool.rs:167-231`), a second line-delimited textual wire format with its own `EvalRequest`/`EvalResponse` enums and per-call `_ => anyhow::bail!("unexpected response to X")` shape-checking repeated in `plan`/`list`/`fingerprint`/`checkpoint`/`resolve` (`pool.rs:233-350`).

**8. `PooledEvalWorker::drop` repeats the graceful-shutdown branch three times** (`pool.rs:681-736`).

**9. `serve_nar_request` is a 200-line function with 6 copies of the abort-and-return idiom** (`socket.rs:98-268`): each error site builds a `reason` string, sends `NarUnavailable`/`NarAbort`, and returns `Err(anyhow!(reason))` by hand.

**10. `on_job_update` is a flat 80-line match that is pure pass-through (`dispatch.rs:703-784`)**, each arm just forwarding to a `scheduler.*` method with bespoke error logging.

## Protocol robustness

**Version negotiation is all-or-nothing and unversioned per-message.** `PROTO_VERSION = 4` (`messages/mod.rs:25`); a mismatch hard-rejects at handshake (`handler/session.rs:114`). There is no capability/feature negotiation for individual messages, so any wire change forces a global version bump and a flag-day upgrade. Forward-compat is ad-hoc: `CacheQuery.mode` documents "Defaults to QueryMode::Normal when deserialized from an older client" (`messages/client.rs:239`), relying on rkyv default-on-missing rather than an explicit scheme.

**Timeouts are a pile of independent, hand-tuned constants that must stay ordered.** `CACHE_QUERY_BUDGET = 45s` server-side (`dispatch.rs:1113`) must stay below `CACHE_QUERY_TIMEOUT = 75s` worker-side (`job.rs:50`), which reuses the same constant for `QueryKnownDerivations` (`job.rs:366`) despite different server budgets; `HANDSHAKE_TIMEOUT = 15s` (`frame.rs:47`); NAR open/chunk timeouts come from config (`socket.rs:81-82`); job wall-clock is a hard-coded `Some(3600)` in `AssignJob` (`dispatch.rs:666`) that the worker then ignores (`worker/dispatch.rs:324`), so the wire field is dead weight. These invariants are documented in prose, not enforced.

**Retry classification is correct but its safety hinges on fragile defaults.** Good part: `CacheError` is a distinct message so an indeterminate cache lookup is retried transiently, never mistaken for "inputs absent" (`dispatch.rs:1139-1149`, `job.rs:404-408`). Fragile part: `BuildFailureKind` derives `Default = Permanent` (`proto.rs:536`) specifically so eval failures and un-downcastable worker errors decode to a terminal value (`worker/dispatch.rs:189`), so a decode glitch or a new error type silently becomes a permanent build failure. `looks_like_oom` string-matching (`build.rs:118`) is the sole signal separating a retryable OOM from a permanent compile error.

**Failure propagation has several silent-drop points.** `on_job_done` maps any non-`BuildError` to `Permanent` with empty `missing_paths` (`worker/dispatch.rs:189`). Late RPC replies whose waiter already cleared are only `debug!`-logged and dropped (`worker/dispatch.rs:697-724`). A frame that fails to rkyv-decode ends the entire connection: `ProtoReader::recv_msg` returns `None` on any decode error (`frame.rs:223,243`), which the worker loop treats as "server closed connection" (`worker/dispatch.rs:100`) and blanket-aborts every in-flight job (`worker/dispatch.rs:141`). There is no per-message NACK or resync.

**Connection liveness is asymmetric.** The server stamps `last_seen` on every inbound frame (`handler/session.rs:338`) but relies on a 10 s worker heartbeat (`worker/dispatch.rs:67`); a worker whose heartbeat task wedges but whose TCP stays open can look alive. WebSocket ping/pong frames are explicitly skipped (`frame.rs:89,229`) rather than used for liveness.

## Refactoring recommendations

Ranked by impact.

**1. Finish the `session`/`client`/`server`/`cap` migration and delete `handler/`'s monolith, do not invent a third design.** The clean target already exists in-tree. Concretely: implement `server/dispatch.rs` (currently a stub) as the single routing loop driven by the `cap::{build,eval,fetch,cache}` traits; port `DispatchContext`'s arms and `MessageHandler`'s arms onto the shared trait objects so routing lives once, not twice; migrate `handler::handle_socket` (used by both `proto_router` and `outbound.rs`) onto it; then delete the duplicate handshake/version-negotiation in `handler/session.rs`, `cache_session.rs`, and the worker handshake. The four hand-synced routing/name tables collapse to one.

**2. Extract a `nar_transfer` module out of both dispatch files.** Move `NarReceiveStore`/`PathState`/`AppendOutcome` (`dispatch.rs:39-240`) and `serve_nar_request`/`invalidate_cached_path`/`send_nar_unavailable` (`socket.rs:72-308`) into a dedicated `handler/nar_transfer.rs`, and collapse `serve_nar_request`'s six abort-and-return copies into one `fn fail_transfer(...)`.

**3. Unify failure classification into one function.** Replace the four scattered sites with a single `fn classify(err: &anyhow::Error) -> BuildError` living beside `BuildError` in a `failure.rs`. Make `on_job_done`'s "unknown error to Permanent" an explicit, logged fallthrough, and reconsider deriving `Default=Permanent` on the wire enum (a decode error should be `Transient`).

**4. Split `executor/build.rs` along its seams.** Pull metric sampling into `executor/build_metrics.rs`; pull the `.drv`-to-harmonia translation (`get_basic_derivation`, `:566-1112`) into `executor/derivation.rs`; keep only `build_derivation` + `ParsedDerivation` + the log-drain state machine in `build.rs`. While there, move `get_basic_derivation`'s per-input `std::fs::read` (`:1011`) onto async IO or `spawn_blocking`.

**5. Collapse the worker's god-context into an owned per-connection struct.** Replace the 18 borrowed fields of `MessageHandler` and the field-by-field reconstruction with one `DispatchState` owned by `run_dispatch_loop` and passed by `&mut`; group the per-job maps into a `JobRegistry` sub-struct. Deletes three `#[allow(too_many_arguments)]`.

**6. Make timeout invariants a single typed config, not scattered constants.** Put `CACHE_QUERY_BUDGET`, `CACHE_QUERY_TIMEOUT`, handshake, and NAR timeouts in one `ProtoTimeouts` struct with a debug-assert that server-budget < worker-timeout. Give `QueryKnownDerivations` its own budget. Delete or honour the dead `AssignJob.timeout_secs` field.

**7. Bring the eval-worker IPC onto rkyv framing.** Swap the JSON-line `request()` (`pool.rs:167`) for the same `u32-len + rkyv bytes` framing the main protocol uses, replace the five per-method `_ => bail!` blocks with one generic `expect_response::<T>()`, and add a version byte to the subprocess handshake.

---

# Part 2 - NAR upload path

NARs (zstd-compressed store-path archives) only ever originate on the worker. The server never packs or compresses a NAR; it only receives, verifies (partially), stores, and records them. Every worker origin funnels into one of two transports, and the route is chosen per-path by the server in its `CacheQuery{Push}` reply (`handler/cache.rs:280` `build_uncached_push_entry`): an S3-backed cache returns a presigned PUT `url`; a local-disk cache returns `url: None`, forcing the WebSocket relay.

## Upload origins (worker) - the answer to "from where are NARs uploaded"

| # | Worker component (file:fn) | What NAR(s) | When / trigger | Transport |
|---|---|---|---|---|
| 1 | `executor/mod.rs:495` `execute_build_job` to `executor/compress.rs:34` `compress_and_push_paths` | Every realised build output plus its full runtime closure (closure-complete), zstd-6 | After a successful local build of all outputs | Per-path via `upload_one_nar` (`mod.rs:198`): presigned or relay |
| 2 | `executor/mod.rs:399` (external_cached branch, gated `:390`) to `proto/nar_import.rs:817` `relay_external_cached_outputs` | Output paths of a substitutable build, sourced from an org upstream cache | Build task flagged `external_cached` (substitutable) | `nar_import.rs:912` `upload_presigned_compressed` or `:925` `push_compressed_direct` |
| 3 | `executor/mod.rs:320-324` `execute_flake_job` (FetchFlake) | Archived flake-source store paths (repo fetched + archived into the store) | `FlakeTask::FetchFlake` completes | Per-path via `upload_one_nar`: presigned or relay |
| 4 | `executor/eval.rs:614` (mid-walk batch) and `:828` (final flush) to `proto/job.rs:486` to `executor/mod.rs:91` `push_drv_closure` | Every produced `.drv`, its transitive `.drv` closure, and all `inputSrcs` (producerless source files, harvested by parsing each `.drv` at `mod.rs:140` `drv_input_sources`) | During eval closure walk, per batch and once at end | Per-path via `upload_one_nar`: presigned or relay |
| 5 | `executor/eval.rs:276` to `proto/job.rs:133` `push_eval_cache` | The eval-cache SQLite blob (not a store-path NAR; not zstd-compressed, raw bytes) | Eval completion, best-effort | `EvalCachePush` grant: presigned PUT, inline `EvalCacheChunk` frames, or Skip |
| 6 | `worker/dispatch.rs:666` `on_presigned_upload` to `nar.rs:482` `upload_presigned` | A single store path the server explicitly asks for | On `ServerMessage::PresignedUpload` | Presigned PUT only |

Notes:
- Origins 1, 3, 4 all converge on `upload_one_nar` (`executor/mod.rs:198`), the single route selector: `Uncached { upload_url: Some }` to `nar::upload_presigned`, `upload_url: None` to `nar::push_direct`, `Cached` to skip. The discrimination is the `CachedPath::as_info()` projection (`gradient-types/src/cached_path_info.rs:64`).
- Origin 6 is effectively dead: `ServerMessage::PresignedUpload` is never emitted by the server build-output path; S3 upload URLs are delivered exclusively via the `CacheQuery{Push}` `CachedPath.url`. `on_presigned_upload` is a second, divergent code path for the same operation that is not exercised.
- Build logs are not NARs: `proto/job.rs:334` `send_log_chunk` streams `ClientMessage::LogChunk` frames; no NAR framing, no zstd.
- Chunking/size constants: `NAR_CHUNK_SIZE = 4 MiB` (`nar.rs:36`), duplicated as `NAR_PUSH_CHUNK_SIZE = 4 MiB` (`session/frame.rs:33`); `MAX_PROTO_MESSAGE_SIZE = 8 MiB` (`frame.rs:41`); `WRITER_QUEUE_DEPTH = 16` caps outbound buffer at ~64 MiB backpressure (`frame.rs:54`); `EVAL_CACHE_CHUNK_SIZE = 4 MiB` (`job.rs:33`).

## Transport A: presigned direct-PUT (S3-backed cache)

```
WORKER                                        SERVER                         OBJECT STORE / DB
------                                        ------                         -----------------
origin 1/3/4  upload_one_nar (mod.rs:213)
  -> nar.rs:482 upload_presigned
       resolve_path_meta (nar.rs:101)  <----- (daemon query, refs+deriver)
       NarByteStream + zstd-6 in memory
       HTTP PUT compressed bytes ------------------------------------------> S3 object
       ClientMessage::NarUploaded ---------> dispatch.rs:965 on_nar_uploaded
       {file_hash,file_size,                    nar.is_active == FALSE
        nar_hash,nar_size,                       (no push stream opened)
        references,deriver}                      => SKIPS size/hash verify + commit
                                                 => mark_nar_stored (handler/nar.rs:90)
                                                      ingest_metadata_only (ingest.rs:97)
                                                        upsert cached_path row ---------> cached_path
                                                        sync_reference_index ----------> cached_path_reference
                                                        insert signatures -------------> cached_path_signature
                                                      UPDATE derivation_output.is_cached=true
```

Route chosen at `cache.rs:288-314`. The server trusts the worker's reported hashes/size verbatim and never HEADs the S3 object (`dispatch.rs:994` `is_active` guard is false, so the verify block `dispatch.rs:994-1035` is skipped straight to `mark_nar_stored` at `:1046`).

## Transport B: server-relayed streaming (local-disk cache, url: None)

```
WORKER                                              SERVER                              STORAGE
------                                              ------                              -------
origin 1/3/4  upload_one_nar (mod.rs:228)
  -> nar.rs:185 push_direct
     resolve_path_meta (nar.rs:199)
     register_push gate (nar_recv.rs:372)
     ClientMessage::NarStreamHeader ----------->  on_push_stream_header (dispatch.rs:897)
       {stream_token = zstd6-fmt1-libN}             note_header -> PartialStore.received_len(token)
     <-- ServerMessage::NarPushResume  <--------   reply received_bytes (partial.rs:68-85)
         {received_bytes}                            (token mismatch => 0, discard partial)
     await_resume (nar_recv.rs:97)
     NarByteStream + zstd-6, 4 MiB chunks
     trim_for_resume (nar.rs:49)
     ClientMessage::NarPush{offset,data} ------>  on_nar_push (dispatch.rs:918)
       (repeat)                                     PartialStore.append (partial.rs:107, contiguous-enforced)
                                                      offset==0 truncates; poison on non-contiguous
     ClientMessage::NarPush{is_final,data:[]} ->  (empty final = no-op server side)
     ClientMessage::NarUploaded --------------->  on_nar_uploaded (dispatch.rs:965)
       {file_hash,file_size,nar_hash,...}           nar.is_active == TRUE:
                                                      committed_len == file_size ? (dispatch.rs:1002)  [size only]
                                                      read_staged -> put_nar_idempotent (ingest.rs:73) -> nars/xx/rest.nar.zst
                                                      nar.finish (discard partial)
                                                    mark_nar_stored -> cached_path + refs + sigs + is_cached
```

Object key: `{prefix}nars/{hash[..2]}/{hash[2..]}.nar.zst` (`gradient-storage/src/nar.rs:110`). The `.partial` staging is namespaced `{peer_id}/{hash}` server-side (`dispatch.rs:91`, `partial.rs:54`) and `{job_id}/{hash}` worker-side on the pull direction (`nar_recv.rs:123`) to fix the concurrent-same-hash non-contiguous-append race.

The substitute relay (origin 2) uses the same two transports but its bytes come from an upstream cache: `nar_import.rs:912` `upload_presigned_compressed` (A, no re-pack) or `:925` `push_compressed_direct` (B).

## Correctness & robustness

**Idempotency / resume (relayed path, solid).** Worker announces a `stream_token = "zstd{level}-fmt1-lib{ver}"` (`nar.rs:41`) and the server replies with bytes already staged (`dispatch.rs:907-915`). `PartialStore::received_len` returns 0 and discards the `.partial` when the persisted `.token` sidecar differs (`partial.rs:68-85`). The worker trims the resend with `trim_for_resume` (`nar.rs:49`). Append is contiguity-enforced: `offset` must equal current length else `bail!` (`partial.rs:107-109`). `put_nar_idempotent` (`ingest.rs:73-95`) handles all four cases (no row / matching hash + object present / differing hash / matching hash + object missing).

**Hash / size verification (the weak spot).**
1. S3 presigned path is entirely unverified (highest severity). `on_nar_uploaded` skips the whole `is_active` commit-and-verify block for S3 (`dispatch.rs:994`), so the server never HEADs the object, never checks size, never rehashes, and goes straight to `mark_nar_stored` (`dispatch.rs:1046`). A `NarUploaded` whose PUT actually failed/truncated creates a `cached_path` row with `is_cached=true` pointing at a missing/corrupt object: the exact zombie-cached-path class that `demote_cached_output` (`cache_storage.rs:251`) exists to repair, but the repairing HEAD is bypassed on this branch.
2. No server-side hash recomputation on either path. Relayed ingest checks only staged-length vs worker-reported `file_size` (`dispatch.rs:1001-1009`); `file_hash`/`nar_hash` are stored verbatim (`ingest.rs:150-152`). The relayed path holds the full bytes in memory (`read_staged`, `dispatch.rs:1010`) and could cheaply recompute but does not.

**Partial-file handling & the concurrent-same-hash race.** Fixed on both directions by peer/job namespacing (`dispatch.rs:91`, `nar_recv.rs:123`, regression test `nar_recv.rs:512-540`). One unenforced invariant: the worker's push gate keys only on `(job_id, store_path)` with a single `oneshot` (`nar_recv.rs:373`); two concurrent pushes of the same path within one job would clobber the gate. Safe today only because pushes are sequential.

**Failure classification and propagation to build outcome.**
- Post-build output upload failure to `BuildError::transient` (`executor/mod.rs:497`): the build retries transiently.
- Substitute relay failure to `classify_substitute_failure` (`executor/mod.rs:177`): only a typed `SubstituteNotOnUpstream` (`nar_import.rs:116`) becomes `SubstituteUnavailable`; every transient is `Transient` so a substitutable build is not needlessly escalated.
- Fetch upload failure to `?` propagation (`mod.rs:323`) fails the flake job.
- Eval `.drv` closure push failure to `?` propagation (`mod.rs:128`, `eval.rs:614`/`:828`) fails the evaluation, by design, so a downstream build never dispatches against an un-pushed source.
- Eval-cache push failure: best-effort, `warn!` only (`eval.rs:278`).
- Server presigned upload handler failure (dead path): `error!` only, no propagation (`dispatch.rs:692`).

## Messiness & code smells

Ranked by impact.

1. **`nar_import.rs` is a 1830-line god module** mixing five concerns: server-to-worker prefetch (`InputPrefetcher`, `:225-751`), the substitute re-upload relay (`relay_external_cached_outputs`, `:817-943`), daemon import (`NarImporter`, `:950-1073`), compression detect/decompress (`:1126-1331`), and `.drv` parse/seed harvesting (`:1214-1307`). It bundles both transfer directions in one file.
2. **Two divergent transport code paths, "URL present -> HTTP else WS" re-expressed at least four times.** `upload_one_nar` (`executor/mod.rs:203-231`), the relay branch (`nar_import.rs:911-937`), the pull-side `classify_cached_entries` (`nar_import.rs:1107-1124`), and the dead `on_presigned_upload` (`worker/dispatch.rs:666`). The server-side commit likewise forks inside one function via `is_active` (`dispatch.rs:994`), which is what hides the missing S3 verification.
3. **`nar.rs` (986 lines) has four near-duplicate push functions.** `push_direct` (`:185-300`), `push_compressed_direct` (`:408-472`), `upload_presigned` (`:482-567`), `upload_presigned_compressed` (`:345-398`), all repeating `ensure_full_store_path`, the header/resume/chunk/`NarUploaded` sequence, and identical `sha256:` + `nix32_encode` hashing. The pack+compress loop is copied verbatim between `push_direct:224-262` and `upload_presigned:506-518`.
4. **`on_nar_uploaded` server ingest is a ~90-line multi-branch function (`dispatch.rs:965-1056`)** interleaving poison handling, size verify, blob commit, DB metadata, and metric recording, with the two transports diverging mid-body. This is where the unverified-S3 gap is buried.
5. **Duplicated hash/size verification and hash parsing in `nar_import.rs`.** `verify_size`/`verify_hash` (`:980-1013`) vs the inline upstream `nar_hash` check in the relay (`:897-903`); `parse_nar_hash_to_bytes` (`:1335-1349`) vs `build_unkeyed_path_info`'s own parse (`:1364-1371`). `verify_hash`'s error message prints a literal `computed sha256:<...>` placeholder instead of the real digest (`:1006`).
6. **Blocking / synchronous CPU work inside async fn.** `decompress`/`decompress_zstd`/`decompress_xz`/`decompress_bzip2` (`:1159-1331`) are synchronous `Read`-based and called directly from async (`:1064`, `:895`), as are full-buffer `Sha256::digest` calls (`:898`, `:999`). Contrast `nar_recv.rs:250,302` which correctly `spawn_blocking`.
7. **Whole-NAR-in-memory, no streaming.** Every push buffers the entire compressed (and often decompressed) NAR in `Vec<u8>` (`upload_presigned:499-515`, relay `:886-907`, `download_one_presigned:178-183`) with `capacity(len*4)` guesses. Up to `PREFETCH_CONCURRENCY = 8` live at once.
8. **`eval_cache_recv.rs` (386) is a structural duplicate of `nar_recv.rs` (557).** Same `Arc<Mutex<Inner>>` + `HashMap` + `oneshot` waiter + timeout + offset-contiguity + `forget_job`. The inline eval-cache push (`job.rs:160-184`) has no resume handshake and no hash/size confirmation.
9. **`NarReceiver` fuses two concerns** (server-to-worker pull streaming and worker-to-server push-resume) in one struct with five parallel `HashMap`s (`nar_recv.rs:54-77`).
10. **Magic constants copy-pasted per module.** Three independent 600s ceilings (`HTTP_DOWNLOAD_TIMEOUT` `nar_import.rs:53`, `NAR_RECV_TIMEOUT` `nar_recv.rs:40`, `EVAL_CACHE_TIMEOUT` `eval_cache_recv.rs:34`); two 4 MiB chunk sizes plus the frame copy; presigned expiry `Duration::from_secs(3600)` duplicated at `cache.rs:522` and `:697`; zstd level `6` hardcoded at 5+ sites.
11. **Inconsistent miss-surfacing.** A 404/410 becomes typed `MissingInputs` on the prefetch path (`nar_import.rs:476-478`) but a plain `bail!` string on the relay path (`:863-864`), so the relay loses the self-heal signal.
12. **`mark_nar_stored` does per-row `UPDATE` in a loop** over `derivation_output` (`handler/nar.rs:128-137`) instead of one bulk `UPDATE ... WHERE hash = $1`. `record_nar_push_metric` re-resolves job-to-org-to-cache on every NAR, duplicating the lookup `mark_nar_stored` just did.

## Refactoring recommendations

Ranked by impact.

1. **Close the S3-verification gap first (correctness, not cosmetics).** In `on_nar_uploaded` (`dispatch.rs:994`), the presigned branch must at minimum HEAD the object and compare its length to the reported `file_size` before `mark_nar_stored`; ideally record a "pending-verify" state and reconcile hash lazily. Reuse `put_nar_idempotent`'s existing `exists` HEAD (`ingest.rs:80`) rather than bypassing it.
2. **Extract a single `nar_transfer` module with one `NarUploader` abstraction.** Define a `NarSource` (built store path via `NarByteStream`, or in-memory compressed bytes from the relay) and a `NarSink` (`Presigned{url,method,headers}` or `Relay{writer,nar_recv}`). Collapse the four `nar.rs` push functions and the relay's two calls into `uploader.send(source, sink)`. Delete `on_presigned_upload` / `ServerMessage::PresignedUpload`.
3. **Separate origin-enumeration from transport.** The five origins should each produce a `Vec<UploadItem>` and hand it to the shared uploader. Today origin logic (closure walking, `.drv` parsing) is tangled into `compress.rs`, `mod.rs::push_drv_closure`, and `nar_import.rs::relay_external_cached_outputs`.
4. **Consolidate verification + hashing into one `NarDigest` helper** used by worker push, worker import, and the new server recompute path. Fix the `verify_hash` placeholder message (`nar_import.rs:1006`) and make the relay reuse it so a 404 becomes typed `MissingInputs` consistently.
5. **Split `nar_import.rs` into `prefetch.rs`, `substitute_relay.rs`, `nar_daemon_import.rs`, and `compression.rs`.** Move all multi-MB decompress/`Sha256::digest` work behind `spawn_blocking` or a streaming decoder.
6. **Extract a generic `ChunkedInlineReceiver<K>`** and build both `NarReceiver` and `EvalCacheReceiver` on it; split `NarReceiver`'s pull vs push-resume responsibilities into two types.
7. **Centralize constants** in one `nar_transfer::config` (chunk size, message ceiling, the three 600s timeouts, presigned expiry, zstd level).
8. **Split `on_nar_uploaded` into `commit_relayed()` and `commit_presigned()`** and bulk the `derivation_output` update into a single SQL statement.

Files central to this path: `backend/gradient-worker/src/executor/{mod.rs,compress.rs,build.rs,fetch.rs,eval.rs}`, `backend/gradient-worker/src/proto/{nar.rs,nar_recv.rs,nar_import.rs,eval_cache_recv.rs,job.rs}`, `backend/gradient-worker/src/worker/dispatch.rs`, `backend/gradient-proto/src/{ingest.rs,session/frame.rs,handler/{nar.rs,dispatch.rs,cache.rs,socket.rs}}`, `backend/gradient-proto/src/messages/{server.rs,client.rs}`, `backend/gradient-storage/src/{nar.rs,partial.rs}`, `backend/gradient-types/src/cached_path_info.rs`.
