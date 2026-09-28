# Messages

Every message on `/proto`, from `backend/gradient-wire/src/messages`. IDs (`job_id`, `dispatch`, peer IDs) are strings on the wire. **Bulk** messages carry payload chunks and travel on the bulk lane, everything else on the control lane.

## Server -> Worker

| Message | Purpose | Key fields |
|---|---|---|
| `AuthChallenge` | Peers that registered this worker | `peers` |
| `InitAck` | Handshake accepted | `version`, `capabilities`, `authorized_peers`, `failed_peers` |
| `AuthUpdate` | Result of a reauth | `authorized_peers`, `failed_peers` |
| `Reject` | Declines the session, then closes | `code`, `reason` |
| `Error` | Protocol error | `code`, `message` |
| `Draining` | Server shutting down; request no more jobs | - |
| `JobListChunk` | Full candidate list, answer to `RequestJobList` | `candidates`, `is_final` |
| `JobOffer` | New candidates, up to 1 000 per message | `candidates` |
| `AssignJob` | Assigns a job | `job_id`, `dispatch`, `job` |
| `AbortJob` | Cancels a job | `job_id`, `reason` |
| `Credential` | Short-lived credential, e.g. an SSH key | `kind`, `data` |
| `NarStreamHeader` | Opens a NAR pull stream | `job_id`, `store_path`, `total_bytes`, `stream_token` |
| `NarPush` (bulk) | NAR pull chunk, 512 KiB zstd | `job_id`, `store_path`, `data`, `offset`, `is_final` |
| `NarUnavailable` | Path cannot be served; no chunks follow | `job_id`, `store_path`, `reason` |
| `NarAbort` | Pull aborted mid-stream | `job_id`, `store_path`, `reason` |
| `EvalCachePullResult` | Answer to `EvalCachePull`: miss, presigned URL or inline stream | `job_id`, `outcome` |
| `EvalCacheChunk` (bulk) | Inline evaluation cache chunk | `job_id`, `data`, `offset`, `is_final` |
| `CacheStatus` | Answer to `CacheQuery` | `query_id`, `cached` |
| `KnownDerivations` | Answer to `QueryKnownDerivations` | `query_id`, `known` |
| `CacheError` | Cache state unknown; the worker retries | `query_id`, `message` |
| `UploadGrant` | Upload admission: skip, relay (with resume offset), presigned PUT or multipart | `request_id`, `target` |
| `UploadCommitted` | Upload outcome: ok, retry or rejected | `request_id`, `outcome` |

## Worker -> Server

| Message | Purpose | Key fields |
|---|---|---|
| `InitConnection` | First message | `version`, `capabilities`, `id` |
| `AuthResponse` | One token per challenged peer | `tokens` |
| `ReauthRequest` | Asks for a new `AuthChallenge` | - |
| `Reject` | Declines after `InitAck`; defined, not sent by the reference worker | `code`, `reason` |
| `WorkerCapabilities` | Systems, features, slots, CPU, RAM, core score | `architectures`, `system_features`, `max_concurrent_builds`, ... |
| `WorkerMetrics` | Load heartbeat | `cpu_usage_pct`, `ram_free_mb`, `disk_speed_mbps`, `network_speed_mbps` |
| `RequestJobList` | Asks for the full candidate list | - |
| `RequestJobChunk` | Score deltas | `scores`, `is_final` |
| `RequestJob` | One free slot of a kind; repeated every 10 s while idle | `kind` (`Flake` or `Build`) |
| `AssignJobResponse` | Accepts or declines an `AssignJob` | `job_id`, `accepted`, `reason` |
| `JobUpdate` | Progress of a job | `job_id`, `dispatch`, `update` |
| `JobCompleted` | Job done, with the phase timeline | `job_id`, `dispatch`, `spans` |
| `JobFailed` | Job failed | `job_id`, `dispatch`, `error`, `kind`, `missing_paths`, `spans` |
| `BuildProgress` | Bytes fetched by a substitute or download | `job_id`, `dispatch`, `build_id`, `downloaded`, `total` |
| `Draining` | Worker draining | - |
| `LogChunk` (bulk) | Build log | `job_id`, `task_index`, `data` |
| `EvalMessage` | Warning or error on the evaluation | `job_id`, `level`, `source`, `message` |
| `NarRequest` | Pull these paths | `job_id`, `paths` |
| `NarRequestResume` | Resume a pull from an offset | `job_id`, `store_path`, `received_bytes`, `stream_token` |
| `EvalCachePull` | Asks for the evaluation cache | `job_id`, `fingerprint` |
| `CacheQuery` | Bulk cache lookup | `job_id`, `query_id`, `paths`, `mode`, `nar_sizes`, `external` |
| `QueryKnownDerivations` | Which `.drv` files the server knows, to prune the walk | `job_id`, `query_id`, `drv_paths` |
| `UploadRequest` | Asks for an upload slot for a NAR or the evaluation cache | `job_id`, `request_id`, `object`, `size` |
| `UploadChunk` (bulk) | Relayed upload bytes | `request_id`, `data`, `offset`, `is_final` |
| `UploadFinished` | Upload done, with NAR metadata | `request_id`, `metadata` |
| `UploadCancel` | Cancels an upload | `request_id` |

## Cache Query Modes

| `mode` | Asks |
|---|---|
| `Normal` | Which paths the caches hold; no URLs |
| `Pull` | Held paths with transfer URLs; without a URL the worker streams with `NarRequest`. With `external`, one named path may come from an upstream |
| `Push` | Which paths still need an upload |

## Evaluation Messages

`EvalMessage` attaches a message to the evaluation of the job, shown on the evaluation page.

- The server stores the message only while the job is active; later messages are dropped.
- An `Error` message fails the evaluation when the evaluation finishes.
- Sources in the reference worker: `fetch` (warnings while fetching inputs) and `build-prefetch`.
