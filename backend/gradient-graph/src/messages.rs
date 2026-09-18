/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What the graph actor is asked to do, and what it answers.

use std::collections::HashSet;

use chrono::NaiveDateTime;
use gradient_db::ReconcileScope;
use gradient_types::MCachedPath;
use gradient_types::ids::{
    BuildAttemptId, CacheId, CachedPathId, DerivationBuildId, DerivationId, DispatchedJobId,
    EvaluationId, ProjectId, TaskId,
};
use gradient_types::proto::{BuildFailureKind, BuildMetrics, BuildOutput, DiscoveredDerivation};

/// One worker batch of discovered derivations plus the one substitution fact the
/// scheduler establishes outside the actor: which of them our own cache already
/// holds whole. What an upstream serves is not asked here - the probe runs for the
/// anchors a demand recompute turns on, and reports through `UpstreamHits`. Paths
/// are in bare `<hash>-<name>` form, because ids are only assigned inside the
/// actor's transaction.
#[derive(Debug, Clone, Default)]
pub struct IngestBatch {
    pub evaluation: EvaluationId,
    pub task: Option<TaskId>,
    pub derivations: Vec<DiscoveredDerivation>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub truly_substituted: HashSet<String>,
}

/// A narinfo hit on a project upstream, persisted onto every `derivation_output`
/// that shares the hash.
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
pub struct IngestReport {
    pub evaluation: EvaluationId,
    pub task: Option<TaskId>,
    /// The batch arrived after the evaluation was aborted and was dropped.
    pub skipped: bool,
    /// Derivations whose full record this batch put in.
    pub walked: usize,
    pub entry_points: Vec<DerivationId>,
}

/// Which caches get a `cached_path_signature` placeholder for a committed path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignTargets {
    ProjectCaches(ProjectId),
    Cache(CacheId),
    None,
}

/// The metadata of a NAR whose bytes are already in `nar_storage`. `store_path`
/// is the full or bare path; `references` are in hash-name form.
#[derive(Debug, Clone)]
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
    /// The object is already in `nar_storage`; false for a relayed NAR staged
    /// for the uploader.
    pub confirmed: bool,
}

/// The uploader's report that a relayed NAR reached the object store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarConfirm {
    pub hash: String,
    pub file_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NarCommitted {
    pub cached_path: CachedPathId,
    /// The `cached_path` row was created by this commit.
    pub created: bool,
    /// `derivation_output` rows now backed by the path.
    pub outputs_marked: u64,
}

/// A state change on the graph. One transaction each.
#[derive(Debug, Clone)]
pub enum Transition {
    /// The worker sent `JobCompleted` for an evaluation: settle the deferred
    /// edges, reconcile the evaluation's closure and move it to `Building`.
    EvalStreamCompleted {
        evaluation: EvaluationId,
    },
    EvalFailed {
        evaluation: EvaluationId,
        error: String,
        kind: BuildFailureKind,
        missing_paths: Vec<String>,
    },
    /// Mark the evaluation aborted and abort every anchor only it still needs.
    AbortEvaluation {
        evaluation: EvaluationId,
    },
    /// The worker reported `Building`; `already_aborted` in the report means
    /// the worker must be told to stop instead.
    BuildStarted {
        anchor: DerivationBuildId,
    },
    BuildOutput {
        anchor: DerivationBuildId,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    },
    BuildCompleted {
        anchor: DerivationBuildId,
    },
    BuildFailed {
        anchor: DerivationBuildId,
        error: String,
        /// The worker's reason with nix's repeated log tail already stripped.
        log_banner: String,
        kind: BuildFailureKind,
        missing_paths: Vec<String>,
    },
    /// A job left the scheduler for a worker: the `build_job`, the open
    /// `build_attempt` and the anchor's `dispatched_at`.
    Dispatched {
        evaluation: EvaluationId,
        anchor: DerivationBuildId,
        dispatched_job: DispatchedJobId,
        substitute: bool,
        build_context: serde_json::Value,
    },
    /// Builds a disconnected worker was running go back to `Queued`.
    OrphanedBuilds {
        anchors: Vec<DerivationBuildId>,
    },
    /// Anchors a dispatch pass just enqueued, plus closure sizes it computed.
    Ready {
        anchors: Vec<DerivationBuildId>,
        closure_sizes: Vec<(DerivationId, i64)>,
    },
    Reconcile {
        scope: ReconcileScope,
    },
    /// Abort the anchors only this evaluation needs; the evaluation row is
    /// already terminal (the trigger path marks it).
    AbortEvaluationAnchors {
        evaluation: EvaluationId,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransitionReport {
    pub aborted_anchors: Vec<DerivationBuildId>,
    pub already_aborted: bool,
    pub substitute_log: Option<SubstituteLog>,
}

/// A completed substitutable anchor whose upstream log the scheduler fetches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstituteLog {
    pub anchor: DerivationBuildId,
    pub derivation: DerivationId,
    pub drv_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequeueScope {
    /// `FailedTransient` anchors whose backoff elapsed go back to `Queued`.
    TransientRetries,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Demotion {
    /// A NAR the index lists is not in storage.
    MissingNar { hash: String },
    /// Operator invalidation: demote, clear the gates, revoke closure claims.
    Path { hash: String },
    /// One cache drops its claim; the path is demoted when it was the last.
    CacheClaim { cache: CacheId, hash: String },
}

#[derive(Debug, Clone, Default)]
pub struct DemoteReport {
    pub producers: Vec<DerivationId>,
    pub cached_path: Option<MCachedPath>,
    pub others_remain: bool,
}

/// A bounded maintenance delete, scanned on the pool and applied here. Each
/// request carries when its scan ran so the actor can re-check what became live
/// since; `Evaluations` needs no such mark, because an evaluation the sweep
/// picked cannot become live again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcRequest {
    Derivations {
        candidates: Vec<DerivationId>,
        scanned_at: NaiveDateTime,
    },
    Paths {
        hashes: Vec<String>,
        scanned_at: NaiveDateTime,
    },
    Evaluations {
        ids: Vec<EvaluationId>,
    },
}

/// What the actor actually removed. The sweep reclaims the objects and log files
/// of exactly these, never of what it asked about: a row the re-check kept live
/// must keep its bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub deleted_derivations: Vec<DerivationId>,
    pub attempt_logs: Vec<BuildAttemptId>,
    pub retired: Vec<String>,
    pub deleted_evaluations: Vec<EvaluationId>,
}
