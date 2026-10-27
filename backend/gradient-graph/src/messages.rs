/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_db::graph::repair::RepairScope;
use gradient_db::status::BuildRefusal;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_types::MCachedPath;
use gradient_types::ids::{
    BuildAttemptId, CacheId, CachedPathId, DerivationBuildId, DerivationId, DispatchedJobId,
    EvaluationId, ExecJobAttemptId, ExecJobId, ProjectId, TaskId,
};
use gradient_wire::types::{BuildFailureKind, BuildMetrics, BuildOutput, DiscoveredDerivation};

/// Paths are in bare `<hash>-<name>` form.
/// Ids are only assigned inside the graph writer's transaction.
#[derive(Debug, Clone, Default)]
pub struct RecordBatch {
    pub evaluation: EvaluationId,
    pub task: Option<TaskId>,
    pub derivations: Vec<DiscoveredDerivation>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpstreamHit {
    pub url: Option<String>,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SignTargets {
    ProjectCaches(ProjectId),
    Cache(CacheId),
    #[default]
    None,
}

#[derive(Debug, Clone, Default)]
pub struct NarCommit {
    pub store_path: String,
    pub file_hash: String,
    pub file_size: i64,
    pub nar_size: i64,
    pub nar_hash: String,
    pub references: Vec<String>,
    pub deriver: Option<String>,
    pub ca: Option<String>,
    pub targets: SignTargets,
    pub confirmed: bool,
    pub built_by_worker: bool,
}

impl NarCommit {
    pub fn from_stored_row(row: &MCachedPath, targets: SignTargets) -> Self {
        Self {
            store_path: row.store_path(),
            file_hash: row.file_hash.clone().unwrap_or_default(),
            file_size: row.file_size.unwrap_or_default(),
            nar_size: row.nar_size.unwrap_or_default(),
            nar_hash: row.nar_hash.clone().unwrap_or_default(),
            references: row
                .references
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            deriver: row.deriver.clone(),
            ca: row.ca.clone(),
            targets,
            confirmed: true,
            built_by_worker: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarCommitted {
    pub cached_path: CachedPathId,
    pub created: bool,
    pub signed: Vec<CacheId>,
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
        metrics: Option<BuildMetrics>,
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
    RetryBuild {
        evaluation: EvaluationId,
        shared_build: DerivationBuildId,
    },
    RequeueImports {
        evaluation: EvaluationId,
        derivations: Vec<DerivationId>,
        without_output_paths: Vec<DerivationId>,
    },
    NotEnoughResources {
        shortages: Vec<RamShortage>,
    },
    SettleExecJobs,
    ExecJobAssigned {
        exec_job: ExecJobId,
        attempt: ExecJobAttemptId,
        number: u32,
        assignment: DispatchedJobId,
        worker: String,
        retries_cap: u32,
    },
    ExecProgramStarted {
        attempt: ExecJobAttemptId,
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
    AbortExecJob {
        exec_job: ExecJobId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamShortage {
    pub shared_build: DerivationBuildId,
    pub evaluation: EvaluationId,
    pub needed_mb: u64,
    pub largest_available_mb: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransitionReport {
    pub aborted_shared_builds: Vec<DerivationBuildId>,
    pub prioritized_shared_builds: Vec<DerivationBuildId>,
    pub already_aborted: bool,
    pub aborted_exec_jobs: Vec<ExecJobId>,
    pub substitute_log: Option<SubstituteLog>,
    pub refusal: Option<BuildRefusal>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Demotion {
    MissingNar { hash: String },
    Path { hash: String },
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
    Evaluations {
        ids: Vec<EvaluationId>,
    },
}

/// The sweep is reclaiming objects and log files of exactly these rows.
/// It never reclaims what it only asked about.
/// A row the re-check kept live must keep its bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub deleted_derivations: Vec<DerivationId>,
    pub attempt_logs: Vec<BuildAttemptId>,
    pub retired: Vec<String>,
    pub deleted_evaluations: Vec<EvaluationId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanCause {
    WorkerLost,
    ClusterAttemptEnded,
    AccessRevoked,
}
