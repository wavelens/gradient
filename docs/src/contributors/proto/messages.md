# Messages

Every message on `/proto`, from `backend/gradient-wire/src/messages`. IDs (`job_id`, `assignment_id`, peer IDs) are strings on the wire. **Bulk** messages carry payload chunks and travel on the bulk lane. All other messages travel on the control lane.

## Server -> Worker

| Message | Purpose | Key fields |
|---|---|---|
| `Authenticate` | First message of a server-dialed session | `worker_id`, `tokens` |
| `AuthChallenge` | Peers that registered this worker | `peers` |
| `InitAck` | Handshake accepted | `capabilities`, `authorized_peers`, `failed_peers` |
| `AuthUpdate` | Result of a reauth | `authorized_peers`, `failed_peers` |
| `Reject` | Declining the session, then closing | `code`, `reason` |
| `Error` | Protocol error | `code`, `message` |
| `Draining` | Server shutting down. Request no more jobs | - |
| `JobListChunk` | Full candidate list, answer to `RequestJobList` | `candidates`, `is_final` |
| `JobOffer` | New candidates, up to 1 000 per message | `candidates` |
| `AssignJob` | Assigning a job. `cluster` is marking one member of a cluster attempt, held until `StartCluster` | `job_id`, `assignment_id`, `job`, `cluster` |
| `AbortJob` | Cancelling a job | `job_id`, `reason` |
| `StartCluster` | Every member accepted. Run the held jobs | `attempt`, `roster` |
| `ClusterSignal` | Control message from another member of the attempt | `attempt`, `from`, `payload` |
| `AbortCluster` | Dropping or aborting every job of the attempt | `attempt`, `reason` |
| `Credential` | Short-lived credential, e.g. an SSH key | `kind`, `data` |
| `NarStreamHeader` | Opening a NAR pull stream | `job_id`, `store_path`, `total_bytes`, `stream_token` |
| `NarPush` (bulk) | NAR pull chunk, 512 KiB zstd | `job_id`, `store_path`, `data`, `offset`, `is_final` |
| `NarUnavailable` | No object in storage for the path. No chunks follow | `job_id`, `store_path`, `reason` |
| `NarAbort` | Pull failed on a storage error or timeout, before or during the stream. Retryable | `job_id`, `store_path`, `reason` |
| `EvalCachePullResult` | Answer to `EvalCachePull`: miss, presigned URL or inline stream | `job_id`, `outcome` |
| `EvalCacheChunk` (bulk) | Inline evaluation cache chunk | `job_id`, `data`, `offset`, `is_final` |
| `CacheStatus` | Answer to `CacheQuery` | `query_id`, `cached` |
| `KnownDerivations` | Answer to `QueryKnownDerivations` | `query_id`, `known` |
| `CacheError` | Cache state unknown. The worker is retrying | `query_id`, `message` |
| `UploadGrant` | Upload admission: skip, passthrough (with resume offset), presigned PUT or multipart | `request_id`, `target` |
| `UploadCommitted` | Upload outcome: ok, retry or rejected | `request_id`, `outcome` |
| `Handover` | Shared worker changing hands. A new `id` wipes the evaluation cache | `id` |

## Worker -> Server

| Message | Purpose | Key fields |
|---|---|---|
| `InitConnection` | First message | `capabilities`, `id` |
| `AuthResponse` | One token per challenged peer | `tokens` |
| `ReauthRequest` | Asking for a new `AuthChallenge` | - |
| `Reject` | Declining a server-dialed session before `InitConnection` (`400`, `401`) | `code`, `reason` |
| `WorkerCapabilities` | Systems, features, slots, CPU, RAM, core score, zone, endpoint | `architectures`, `system_features`, `max_concurrent_builds`, `zone`, `endpoint`, ... |
| `WorkerMetrics` | Load heartbeat | `cpu_usage_pct`, `ram_free_mb`, `disk_speed_mbps`, `upload_speed_mbps`, `download_speed_mbps` |
| `RequestJobList` | Asking for the full candidate list | - |
| `RequestJobChunk` | Score deltas | `scores`, `is_final` |
| `RequestJob` | One free slot of a kind. Repeated every 10 s while idle | `kind` (`Flake` or `Build`) |
| `AssignJobResponse` | Accepting or declining an `AssignJob` | `job_id`, `accepted`, `reason` |
| `ClusterSignal` | Control message to one member (`to`) or every other member (`to` unset) of a started attempt | `attempt`, `to`, `payload` |
| `JobUpdate` | Progress of a job | `job_id`, `assignment_id`, `update` |
| `JobCompleted` | Job done, with the phase timeline | `job_id`, `assignment_id`, `spans` |
| `JobFailed` | Job failed, with the metrics of a failed build | `job_id`, `assignment_id`, `error`, `kind`, `missing_paths`, `spans`, `metrics` |
| `BuildProgress` | Bytes and paths of a build's prefetch, download or upload | `job_id`, `assignment_id`, `build_id`, `phase`, `bytes_done`, `bytes_total`, `paths_done`, `paths_total` |
| `EvalProgress` | Flake input downloads or live thunks of an eval job, at most once per second | `job_id`, `assignment_id`, `progress` |
| `Draining` | Worker draining | - |
| `LogChunk` (bulk) | Build log | `job_id`, `task_index`, `data` |
| `EvalMessage` | Warning or error on the evaluation | `job_id`, `level`, `source`, `message` |
| `NarRequest` | Pull these paths | `job_id`, `paths` |
| `NarRequestResume` | Resume a pull from an offset | `job_id`, `store_path`, `received_bytes`, `stream_token` |
| `EvalCachePull` | Asking for the evaluation cache | `job_id`, `fingerprint` |
| `CacheQuery` | Bulk cache lookup | `job_id`, `query_id`, `paths`, `mode`, `nar_sizes`, `external` |
| `QueryKnownDerivations` | The `.drv` files known to the server, for shortening the walk | `job_id`, `query_id`, `drv_paths` |
| `UploadRequest` | Asking for an upload slot for a NAR or the evaluation cache | `job_id`, `request_id`, `object`, `size` |
| `UploadChunk` (bulk) | Passthrough upload bytes | `request_id`, `data`, `offset`, `is_final` |
| `UploadFinished` | Upload done, with NAR metadata | `request_id`, `metadata` |
| `UploadCancel` | Cancelling an upload | `request_id` |
| `HandoverDone` | `Handover` applied | - |

## Cache Query Modes

| `mode` | Request |
|---|---|
| `Normal` | The paths held by the caches. No URLs |
| `Pull` | Held paths with transfer URLs. The worker is streaming a path without a URL through `NarRequest`. One named path may come from an upstream cache with `external` |
| `Push` | The paths still needing an upload |

## Evaluation Messages

An `EvalMessage` can attach a message to the evaluation of the job, shown on the evaluation page.

- The server will store the message only while the job is active.
- The server will drop later messages.
- An `Error` message will fail the evaluation once the evaluation is finished.
- Sources in the reference worker: `fetch` (warnings while fetching inputs) and `build-prefetch`.
