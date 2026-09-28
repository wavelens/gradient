# Messages

Every message type on the wire, in both directions.

## Server -> Worker

```rust
enum ServerMessage {
    // Handshake + auth
    AuthChallenge { peers: Vec<Uuid> },          // "these peers registered you - send tokens"
    InitAck { version: u16, capabilities: GradientCapabilities, authorized_peers: Vec<Uuid>, failed_peers: Vec<FailedPeer> },
    AuthUpdate { authorized_peers: Vec<Uuid>, failed_peers: Vec<FailedPeer> },  // reauth result
    Reject { code: u16, reason: String },       // decline connection (closes after send)
    Error { code: u16, message: String },

    // Job dispatch
    JobOffer { candidates: Vec<JobCandidate> },  // delta-only: only new candidates; paginated at 1 000
    RevokeJob { job_ids: Vec<Uuid> },            // remove candidates assigned to another worker
    AssignJob { job_id: Uuid, dispatch: Uuid, job: Job },  // dispatch is the dispatched_job id the server minted for this hand-out; the server drops a report whose dispatch is not the one it assigned
    AbortJob { job_id: Uuid, reason: String },
    RequestAllScores,                           // startup-only: ask worker to re-send all scores once
    Draining,                                   // server shutting down; finish and report in-flight work, then reconnect with backoff

    // Credentials (sent before or alongside AssignJob)
    Credential { kind: CredentialKind, data: Vec<u8> },

    // NAR transfer - direct mode (pull)
    NarPush { job_id: Uuid, store_path: String, data: Vec<u8>, offset: u64, is_final: bool },

    // Upload admission (see "Upload admission")
    UploadGrant { request_id: u64, target: GrantTarget },          // Skip | Relay { resume_offset } | Put { url } | Multipart(..)
    UploadCommitted { request_id: u64, outcome: UploadOutcome },   // Ok | Retry { reason } | Rejected { reason }

    // NAR transfer - failure signals (responses to NarRequest)
    NarUnavailable { job_id: Uuid, store_path: String, reason: String },  // server cannot serve; no chunks will follow
    NarAbort       { job_id: Uuid, store_path: String, reason: String },  // in-flight transfer aborted; discard partial buffer

    // Cache queries
    CacheStatus { job_id: String, cached: Vec<CachedPath> },   // response to CacheQuery
    CacheError { job_id: String, message: String },            // CacheQuery indeterminate (DB error / over budget) -> worker retries

    // BFS pruning (EvaluateDerivations)
    /// Response to `QueryKnownDerivations`.  `known` is the subset of the
    /// requested `.drv` paths that are already in the server's derivation table
    /// for the owning project.
    KnownDerivations { query_id: String, known: Vec<String> },
}

struct FailedPeer { peer_id: Uuid, reason: String }
enum CredentialKind { SshKey }
```

## Worker -> Server

```rust
enum ClientMessage {
    // Handshake + auth
    InitConnection { version: u16, capabilities: GradientCapabilities, id: Uuid },
    AuthResponse { tokens: Vec<(String, String)> },  // [(peer_id, token), ...]
    ReauthRequest,                              // ask server to re-send AuthChallenge
    Reject { code: u16, reason: String },       // decline connection after InitAck
    WorkerCapabilities { architectures: Vec<String>, system_features: Vec<String>, max_concurrent_builds: u32, cpu_count: u32, ram_total_mb: u64, cpu_core_score: u32 },
    WorkerMetrics { cpu_usage_pct: f32, ram_free_mb: u64, disk_speed_mbps: Option<f32>, network_speed_mbps: Option<f32> },
    AssignJobResponse { job_id: Uuid, accepted: bool, reason: Option<String> },

    // Job dispatch
    RequestJobChunk {                           // delta-only: new or changed scores; paginated at 1 000
        scores: Vec<CandidateScore>,
        is_final: bool,                         // true on last chunk of each scoring pass
    },
    RequestJob { kind: JobKind },               // "I have capacity for one job" - re-sent every 10s as heartbeat
    RequestAllCandidates,                       // startup-only: ask server to re-send all active candidates once
    JobUpdate { job_id: Uuid, dispatch: Uuid, update: JobUpdateKind },  // dispatch echoes the AssignJob id
    JobCompleted { job_id: Uuid, dispatch: Uuid, spans: Vec<JobPhaseSpan> },  // all steps done; results already sent via JobUpdate. Per-build metrics travel on JobUpdate::BuildOutput
    JobFailed { job_id: Uuid, dispatch: Uuid, error: String, kind: BuildFailureKind, missing_paths: Vec<String>, spans: Vec<JobPhaseSpan> }, // missing_paths set only for kind=InputsUnavailable
    Draining,                                   // no more jobs; finishing in-flight work then disconnecting

    // Streaming
    LogChunk { job_id: Uuid, task_index: u32, data: Vec<u8> },
    BuildProgress { job_id: Uuid, dispatch: Uuid, build_id: Uuid, downloaded: u64, total: Option<u64> },

    // NAR transfer
    NarRequest { job_id: Uuid, paths: Vec<String> },    // "send me these paths"

    // Upload admission (see "Upload admission")
    UploadRequest { job_id: String, request_id: u64, object: UploadObject, size: u64 },  // Nar { store_path } | EvalCache { fingerprint }
    UploadChunk { request_id: u64, data: Vec<u8>, offset: u64, is_final: bool },       // relayed bytes, bulk lane
    UploadFinished { request_id: u64, metadata: UploadMetadata },  // Nar(file_hash, file_size, nar_size, nar_hash,
                                                                   //     references, deriver, ca, multipart) | EvalCache { size_bytes }
    UploadCancel { request_id: u64 },

    // Cache queries
    CacheQuery { job_id: String, paths: Vec<String>, mode: QueryMode },  // see QueryMode

    // BFS pruning (EvaluateDerivations)
    /// Ask the server which of the given `.drv` paths are already recorded in
    /// its derivation table for the project that owns `job_id`.  The server responds
    /// with `KnownDerivations`.  The worker uses this to skip re-traversing
    /// subtrees that were fully recorded during a previous evaluation.
    QueryKnownDerivations { job_id: String, query_id: String, drv_paths: Vec<String> },

    /// Surface an infrastructure-level message on the evaluation that owns
    /// the given `job_id`.  The server resolves the active job -> evaluation
    /// and inserts a row into `evaluation_message` so operators see
    /// transport / prefetch / cache problems on the evaluation page directly,
    /// without drilling into individual build logs.
    ///
    /// **Not** used for build compile failures or user-initiated aborts -
    /// those stay confined to the build log and `JobFailed`.  Use this only
    /// for signals the user could not diagnose from a single build's output.
    EvalMessage {
        job_id: String,
        level: EvalMessageLevel,  // Error | Warning | Notice
        source: String,           // e.g. "build-prefetch", "nar-import"
        message: String,
    },
}

enum QueryMode { Normal, Pull, Push }  // default: Normal
enum EvalMessageLevel { Error, Warning, Notice }
```

## EvalMessage

Workers emit `EvalMessage` to attach an error / warning / notice to the
evaluation that owns the active job.  The server:

1. Looks up `active_job(job_id)` in the scheduler's job tracker.
2. If found, resolves `PendingJob::evaluation_id()` (both eval and build
   jobs carry this) and inserts into `evaluation_message` via
   `db::insert_evaluation_message`.
3. If the job is not active (already completed or evicted), the message is
   silently dropped - late infra signals for a finished job have no useful
   destination.

Because `check_evaluation_done` already treats any error-level
`evaluation_message` row as a failure signal, an `EvalMessage { level: Error }`
arriving during a build is enough to mark the whole evaluation `Failed`
once its builds settle.

Typical producer sites today:
- worker `prefetch_inputs` (source `"build-prefetch"`) when an input NAR
  download or daemon import can't complete - the build would otherwise die
  with "dependency does not exist" in the build log with no evaluation-level
  breadcrumb.
