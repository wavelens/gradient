/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What the graph actor is asked to do, and what it answers.

use std::collections::{HashMap, HashSet};

use gradient_types::ids::{DerivationBuildId, DerivationId, EvaluationId, TaskId};
use gradient_types::proto::{BuildFailureKind, DiscoveredDerivation};

/// One worker batch of discovered derivations plus the substitution facts the
/// scheduler established outside the actor (cache reads and the upstream probe).
/// Paths are in bare `<hash>-<name>` form; the facts are keyed by drv path and
/// by output hash because ids are only assigned inside the actor's transaction.
#[derive(Debug, Clone, Default)]
pub struct IngestBatch {
    pub evaluation: EvaluationId,
    pub task: Option<TaskId>,
    pub derivations: Vec<DiscoveredDerivation>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub truly_substituted: HashSet<String>,
    pub upstream_substitutable: HashSet<String>,
    pub upstream_hits: HashMap<String, UpstreamHit>,
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
    pub new_derivations: usize,
    pub entry_points: Vec<DerivationId>,
}

/// A state change on the graph. One transaction each.
#[derive(Debug, Clone)]
pub enum Transition {
    /// The worker sent `JobCompleted` for an evaluation: settle the deferred
    /// edges, reconcile the evaluation's closure and move it to `Building`.
    EvalStreamCompleted { evaluation: EvaluationId },
    EvalFailed {
        evaluation: EvaluationId,
        error: String,
        kind: BuildFailureKind,
        missing_paths: Vec<String>,
    },
    /// Mark the evaluation aborted and abort every anchor only it still needs.
    AbortEvaluation { evaluation: EvaluationId },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransitionReport {
    pub aborted_anchors: Vec<DerivationBuildId>,
}
