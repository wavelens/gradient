/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_db::exec_jobs::run_again::RunRefusal;
use gradient_db::graph::consistency::{
    BuildGraphConsistencyCheckStep, ConsistencyCheckRepairScopeChunkProgress,
};
use gradient_db::graph::repair::RepairScope;
use gradient_db::status::BuildRefusal;
use gradient_db::store_path_content_variant_trust::ProvenWorkerCredentialIds;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_entity::store_path_content_producer::CacheUploadChannel;
use gradient_storage::ServerComputedNarAndCompressedFileHashesAndSizes;
use gradient_types::MCachedPath;
use gradient_types::ids::{
    ApiId, BuildAttemptId, CacheId, CacheUpstreamId, CachedPathId, DerivationBuildId, DerivationId,
    DispatchedJobId, EvaluationId, ExecJobAttemptId, ExecJobId, ProjectId, TaskId, UserId,
};
use gradient_wire::types::{
    BuildFailureKind, BuildMetrics, BuildOutput, DiscoveredDerivation, ExecSpec, License,
};

use crate::derivation_closure_input_addressed_output_verification::VerifiedDerivationHashModulo;
use crate::read_derivation_from_trusted_nar::StorePathVerifiedDerivationRecord;

#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationBatchDerivation {
    pub store_path_verified_derivation_record: StorePathVerifiedDerivationRecord,
    pub attr: String,
    pub ifd: bool,
    pub licenses: Vec<License>,
    pub exec: Option<ExecSpec>,
    pub closure_checked_hash_modulo: Option<VerifiedDerivationHashModulo>,
}

impl EvaluationBatchDerivation {
    pub fn to_discovered_derivation(&self) -> DiscoveredDerivation {
        DiscoveredDerivation {
            attr: self.attr.clone(),
            ifd: self.ifd,
            licenses: self.licenses.clone(),
            exec: self.exec.clone(),
            ..self
                .store_path_verified_derivation_record
                .to_discovered_derivation()
        }
    }
}

/// Paths are in bare `<hash>-<name>` form.
/// Ids are only assigned inside the graph writer's transaction.
#[derive(Debug, Clone, Default)]
pub struct RecordBatch {
    pub evaluation: EvaluationId,
    pub task: Option<TaskId>,
    pub derivations: Vec<EvaluationBatchDerivation>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub continuation: Option<ExecJobId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationOutcome {
    Done,
    Retry,
    Failed { error: String },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpstreamHit {
    pub url: Option<String>,
    pub verified_by: Option<CacheUpstreamId>,
    pub nar_hash: Option<String>,
    pub file_hash: Option<String>,
    pub file_size: Option<i64>,
    pub nar_size: Option<i64>,
    pub references: Option<String>,
    pub deriver: Option<String>,
    pub ca: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordReport {
    pub evaluation: EvaluationId,
    pub task: Option<TaskId>,
    pub skipped: bool,
    pub walked: usize,
    pub entry_points: Vec<DerivationId>,
    pub exec_entry_points: usize,
    pub to_probe: Vec<DerivationId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredContentProducer {
    Worker {
        project: ProjectId,
        credentials: ProvenWorkerCredentialIds,
        dispatched_job: Option<DispatchedJobId>,
    },
    Upload {
        caches: Vec<CacheId>,
        user: Option<UserId>,
        api_key: Option<ApiId>,
        channel: CacheUploadChannel,
    },
    ProjectUserContentUpload {
        project: ProjectId,
        user: UserId,
        channel: CacheUploadChannel,
    },
}

#[derive(Debug, Clone)]
pub enum NarCommitContent {
    ServerComputedNarAndCompressedFileHashesAndSizes(
        ServerComputedNarAndCompressedFileHashesAndSizes,
    ),
    ExistingServerVerifiedCachedPathId(CachedPathId),
}

#[derive(Debug, Clone)]
pub struct NarCommit {
    pub store_path: String,
    pub content: NarCommitContent,
    pub references: Vec<String>,
    pub deriver: Option<String>,
    pub ca: Option<String>,
    pub producer: StoredContentProducer,
}

impl NarCommit {
    pub fn content_address(&self) -> Option<&str> {
        self.ca.as_deref().filter(|ca| !ca.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarCommitted {
    pub cached_path: CachedPathId,
    pub nar_object_storage_key: String,
    pub created: bool,
    pub signed: Vec<CacheId>,
}

#[derive(Debug, Clone)]
pub struct FailedBuildDispatchReport {
    pub dispatched_job: DispatchedJobId,
    pub substitute: bool,
    pub build_context: serde_json::Value,
    pub metrics: Option<BuildMetrics>,
}

#[derive(Debug, Clone)]
pub enum Transition {
    EvalStreamCompleted {
        evaluation: EvaluationId,
    },
    EvalFailed {
        evaluation: EvaluationId,
        error: String,
        kind: BuildFailureKind,
        missing_paths: Vec<String>,
    },
    BuildStarted {
        shared_build: DerivationBuildId,
        dispatched_job: DispatchedJobId,
        substitute: bool,
        build_context: serde_json::Value,
    },
    BuildOutput {
        shared_build: DerivationBuildId,
        dispatched_job: DispatchedJobId,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    },
    BuildCompleted {
        shared_build: DerivationBuildId,
    },
    BuildFailed {
        shared_build: DerivationBuildId,
        error: String,
        log_banner: String,
        kind: BuildFailureKind,
        missing_paths: Vec<String>,
        dispatch_that_reported_failure: Option<FailedBuildDispatchReport>,
    },
    Assigned {
        shared_build: DerivationBuildId,
    },
    OrphanedBuilds {
        shared_builds: Vec<DerivationBuildId>,
        cause: OrphanCause,
        observed_peak_ram_mb: std::collections::HashMap<DerivationBuildId, u64>,
    },
    Ready {
        shared_builds: Vec<DerivationBuildId>,
        closure_sizes: Vec<(DerivationId, i64)>,
    },
    Repair {
        scope: RepairScope,
    },
    RunConsistencyCheckStep {
        step: BuildGraphConsistencyCheckStep,
        progress: ConsistencyCheckRepairScopeChunkProgress,
    },
    DemoteCompletedBuildsMissingNeededStoredOutputCopies,
    SignStoredInputsIntoReadWriteBinaryCache {
        signed_dependency_output_rows_to_insert:
            Vec<crate::claims::SignatureRowForGraphWriterInsert>,
    },
    AbortEvaluationSharedBuilds {
        evaluation: EvaluationId,
        hard: bool,
    },
    PrioritizeEvaluation {
        evaluation: EvaluationId,
    },
    PrioritizeBuild {
        shared_build: DerivationBuildId,
    },
    AbortBuild {
        evaluation: EvaluationId,
        shared_build: DerivationBuildId,
    },
    AbortBuildForAllEvaluations {
        evaluation: EvaluationId,
        shared_build: DerivationBuildId,
    },
    RetryBuild {
        evaluation: EvaluationId,
        shared_build: DerivationBuildId,
    },
    RequeueImports {
        evaluation: EvaluationId,
        derivations: Vec<DerivationId>,
        without_output_paths: Vec<DerivationId>,
    },
    SettleExecJobs,
    ExecJobAssigned {
        exec_job: ExecJobId,
        attempt: ExecJobAttemptId,
        number: u32,
        assignment: DispatchedJobId,
        worker: String,
        retries_cap: u32,
        token_hash: String,
        token_expires_at: NaiveDateTime,
        master_port: Option<u16>,
    },
    ExecClusterSettled {
        nodes: Vec<ExecNodeEnd>,
        next: ClusterNext,
    },
    ContinuationEnded {
        exec_job: ExecJobId,
        outcome: ContinuationOutcome,
    },
    ExecProgramStarted {
        attempt: ExecJobAttemptId,
    },
    ExecProgramExited {
        attempt: ExecJobAttemptId,
        code: i32,
    },
    ExecPhase {
        attempt: ExecJobAttemptId,
        name: String,
        payload: Vec<u8>,
    },
    ExecAttemptEnded {
        attempt: ExecJobAttemptId,
        reason: AttemptEndReason,
        message: Option<String>,
    },
    RunExecJob {
        exec_job: ExecJobId,
    },
    AbortExecJob {
        exec_job: ExecJobId,
    },
    FailExecJob {
        exec_job: ExecJobId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecNodeEnd {
    pub attempt: ExecJobAttemptId,
    pub reason: AttemptEndReason,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterNext {
    Restart { consume_retry: bool },
    End,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransitionReport {
    pub aborted_shared_builds: Vec<DerivationBuildId>,
    pub prioritized_shared_builds: Vec<DerivationBuildId>,
    pub already_aborted: bool,
    pub aborted_exec_jobs: Vec<ExecJobId>,
    pub aborted_continuations: Vec<ExecJobId>,
    pub substitute_log: Option<SubstituteLog>,
    pub refusal: Option<BuildRefusal>,
    pub exec_run_refusal: Option<RunRefusal>,
    pub consistency_check_progress: Option<ConsistencyCheckRepairScopeChunkProgress>,
}

impl From<gradient_db::status::BuildRefusal> for TransitionReport {
    fn from(refusal: gradient_db::status::BuildRefusal) -> Self {
        Self {
            refusal: Some(refusal),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstituteLog {
    pub shared_build: DerivationBuildId,
    pub derivation: DerivationId,
    pub drv_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequeueScope {
    TransientRetries,
    NeededOutputs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Demotion {
    MissingNarOfContentVariant { content_variant: CachedPathId },
    CacheClaim { cache: CacheId, hash: String },
}

#[derive(Debug, Clone, Default)]
pub struct DemoteReport {
    pub producers: Vec<DerivationId>,
    pub cached_path: Option<MCachedPath>,
    pub others_remain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcRequest {
    Derivations {
        candidates: Vec<DerivationId>,
    },
    Paths {
        hashes: Vec<String>,
        scanned_at: NaiveDateTime,
    },
    UnusedCacheClaims {
        hashes: Vec<String>,
        unused_since: NaiveDateTime,
        scanned_at: NaiveDateTime,
    },
    /// The storage probe can already detect a missing NAR file, so live paths lose that row too.
    RetireCachedPathsWithMissingNarFiles {
        ids: Vec<CachedPathId>,
    },
    Evaluations {
        ids: Vec<EvaluationId>,
    },
    OrphanObjects {
        object_storage_keys: Vec<String>,
    },
}

/// The sweep is reclaiming objects and log files of exactly these rows.
/// It never reclaims what it only asked about.
/// A row the re-check kept live must keep its bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub deleted_derivations: Vec<DerivationId>,
    pub attempt_logs: Vec<BuildAttemptId>,
    pub unreferenced_cached_path_object_storage_keys: Vec<String>,
    pub unreferenced_orphan_object_storage_keys: Vec<String>,
    pub deleted_claim_row_count: u64,
    pub deleted_evaluations: Vec<EvaluationId>,
    pub deleted_exec_jobs: Vec<ExecJobId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanCause {
    WorkerLost,
    ClusterAttemptEnded,
    AccessRevoked,
}
