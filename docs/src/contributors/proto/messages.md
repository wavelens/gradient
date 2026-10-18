# Messages

The message definitions live in `backend/gradient-wire/src/messages` and cover everything sent over `/proto`. All identifiers such as `job_id`, `assignment_id` and peer IDs travel as strings. **Bulk** messages carry payload chunks and use the bulk lane. Every other message uses the control lane.

## Server -> Worker

| Message | Purpose | Key fields |
|---|---|---|
| `Authenticate` | Open a server-dialed session with this first message | `worker_id`, `tokens` |
| `AuthChallenge` | Peers that registered this worker | `peers` |
| `InitAck` | Handshake accepted | `capabilities`, `authorized_peers`, `failed_peers` |
| `AuthUpdate` | Outcome of a reauth | `authorized_peers`, `failed_peers` |
| `Reject` | Decline the session and close it | `code`, `reason` |
| `Error` | Protocol error | `code`, `message` |
| `Draining` | Session will end soon. Send no more job requests | - |
| `JobListChunk` | Complete candidate list, sent in reply to `RequestJobList` | `candidates`, `is_final` |
| `JobOffer` | New candidates, at most 1 000 per message | `candidates` |
| `AssignJob` | Hand a job to the worker. `cluster` marks one member of a cluster attempt and hold it until `StartCluster` | `job_id`, `assignment_id`, `job`, `cluster` |
| `AbortJob` | Cancel a job | `job_id`, `reason` |
| `StartCluster` | All members accepted. Run the held jobs | `attempt`, `roster` |
| `ClusterSignal` | Control message sent by another member of the attempt | `attempt`, `from`, `payload` |
| `AbortCluster` | Drop or abort all jobs of the attempt | `attempt`, `reason` |
| `Credential` | Short-lived credential, for example an SSH key | `kind`, `data` |
| `NarStreamHeader` | Open a NAR pull stream | `job_id`, `store_path`, `total_bytes`, `stream_token` |
| `NarPush` (bulk) | One NAR pull chunk of `nar.chunkBytes` zstd | `job_id`, `store_path`, `data`, `offset`, `is_final` |
| `NarUnavailable` | Storage holds no object for the path. No chunks will follow | `job_id`, `store_path`, `reason` |
| `NarAbort` | Pull failed before or during the stream on a storage error or timeout. Retryable | `job_id`, `store_path`, `reason` |
| `EvalCachePullResult` | Reply to `EvalCachePull`: miss, presigned URL or inline stream | `job_id`, `outcome` |
| `EvalCacheChunk` (bulk) | One inline evaluation cache chunk | `job_id`, `data`, `offset`, `is_final` |
| `CacheStatus` | Reply to `CacheQuery` | `query_id`, `cached` |
| `KnownDerivations` | Reply to `QueryKnownDerivations` | `query_id`, `known` |
| `CacheError` | Cache state unknown. The worker will retry | `query_id`, `message` |
| `UploadGrant` | Upload admission: skip, passthrough (with resume offset), presigned PUT or multipart | `request_id`, `target` |
| `UploadCommitted` | Upload outcome: ok, retry or rejected | `request_id`, `outcome` |
| `ImportResult` | Reply to `ImportRequest`, sent after the imported derivations finished: completed, failed with the build, or unknown | `job_id`, `request_id`, `outcome` |
| `InputResult` | Reply to `InputRequest`: the fetch answer with the store path, or the download error | `job_id`, `request_id`, `outcome` |
| `InputProgress` | Download bytes of the input fetch an `InputRequest` waits for | `job_id`, `request_id`, `bytes_done`, `bytes_total` |
| `FetchInput` | Download one input for a project and push the result to the cache. Carries the project SSH key only for a worker with `secrets` | `fetch_id`, `project`, `request`, `ssh_key` |

## Worker -> Server

| Message | Purpose | Key fields |
|---|---|---|
| `InitConnection` | Sent first to open the connection | `capabilities`, `id` |
| `AuthResponse` | Carries one token for each challenged peer | `tokens` |
| `ReauthRequest` | Requests a fresh `AuthChallenge` | - |
| `Reject` | Declines a session dialed by the server before `InitConnection` (`400`, `401`) | `code`, `reason` |
| `WorkerCapabilities` | Describes the worker: systems, features, slots, CPU, RAM, core score, zone, endpoint | `architectures`, `system_features`, `max_concurrent_builds`, `zone`, `endpoint`, ... |
| `WorkerMetrics` | Heartbeat with current load | `cpu_usage_pct`, `ram_free_mb`, `disk_speed_mbps`, `upload_speed_mbps`, `download_speed_mbps`, `build_peak_ram_mb` (since 33) |
| `RequestJobList` | Requests the full candidate list | - |
| `RequestJobChunk` | Carries score deltas | `scores`, `is_final` |
| `RequestJob` | Claims one free slot of a given kind. A worker repeats this every 10 s while idle | `kind` (`Flake` or `Build`) |
| `AssignJobResponse` | Accepts or declines an `AssignJob` | `job_id`, `accepted`, `reason` |
| `ClusterSignal` | Sends a control message to a single member (`to` set) or to all other members (`to` unset) of a started attempt | `attempt`, `to`, `payload` |
| `JobUpdate` | Reports job progress | `job_id`, `assignment_id`, `update` |
| `JobCompleted` | Marks a job done and carries the phase timeline | `job_id`, `assignment_id`, `spans` |
| `JobFailed` | Marks a job failed and carries the metrics of the failed build | `job_id`, `assignment_id`, `error`, `kind`, `missing_paths`, `spans`, `metrics` |
| `BuildProgress` | Reports bytes and paths for a build's prefetch, download or upload | `job_id`, `assignment_id`, `build_id`, `phase`, `bytes_done`, `bytes_total`, `paths_done`, `paths_total` |
| `EvalProgress` | Reports flake input downloads or live thunks of an eval job, at most once per second | `job_id`, `assignment_id`, `progress` |
| `Draining` | Tells the server the worker will stop. No further job assignments | - |
| `LogChunk` (bulk) | Streams the build log | `job_id`, `task_index`, `data` |
| `EvalMessage` | Carries a warning or error from the evaluation | `job_id`, `level`, `source`, `message` |
| `NarRequest` | Pulls the given paths | `job_id`, `paths` |
| `NarRequestResume` | Resumes a pull from a byte offset | `job_id`, `store_path`, `received_bytes`, `stream_token` |
| `EvalCachePull` | Requests the evaluation cache | `job_id`, `fingerprint` |
| `CacheQuery` | Performs a bulk cache lookup | `job_id`, `query_id`, `paths`, `mode`, `nar_sizes`, `external` |
| `QueryKnownDerivations` | Asks which `.drv` files the server knows, to shorten the walk | `job_id`, `query_id`, `drv_paths` |
| `UploadRequest` | Requests an upload slot for a NAR or the evaluation cache | `job_id`, `request_id`, `object`, `size` |
| `UploadChunk` (bulk) | Streams upload bytes through the server | `request_id`, `data`, `offset`, `is_final` |
| `UploadFinished` | Marks an upload done and carries NAR metadata | `request_id`, `metadata` |
| `UploadCancel` | Cancels an upload in progress | `request_id` |
| `ImportRequest` | Builds the given derivations now, because the evaluation needs them | `job_id`, `request_id`, `drv_paths` |
| `InputRequest` | Asks for an input missing from the store and the cache, because the evaluation needs the input | `job_id`, `request_id`, `request` |
| `InputFetchProgress` | Reports download bytes of a `FetchInput` | `fetch_id`, `bytes_done`, `bytes_total` |
| `InputFetched` | Reply to `FetchInput` after the push: the fetch answer or the download error | `fetch_id`, `outcome` |

## Cache Query Modes

| `mode` | Request |
|---|---|
| `Normal` | The paths that live in the caches. The response contains no URLs |
| `Pull` | The held paths, each with a transfer URL. The worker streams a path without a URL via `NarRequest`. One named path can originate from an upstream cache marked `external` |
| `Push` | The paths that still require an upload |

## Evaluation Messages

An `EvalMessage` can attach a message to the evaluation of the job, shown on the evaluation page.

- The server will store the message only while the job is active.
- The server will drop later messages.
- An `Error` message will fail the evaluation once the evaluation is finished.
- Sources in the reference worker: `fetch` (warnings while fetching inputs) and `build-prefetch`.
