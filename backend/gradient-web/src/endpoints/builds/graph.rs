/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{ApiKeyContext, MaybeApiKey, MaybeUser};
use crate::error::WebResult;
use crate::helpers::{OptionExt, ok_json};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::EntityTrait;
use sea_orm::{ColumnTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::BuildAccessContext;

// ── Dependency graph helpers ──────────────────────────────────────────────────

pub(super) async fn authorize_build_opt(
    state: &Arc<ServerState>,
    build_id: BuildJobId,
    maybe_user: &Option<MUser>,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<()> {
    BuildAccessContext::load(state, build_id, maybe_user, api_key)
        .await
        .map(|_| ())
}

/// A build_job paired with its anchor's status, for one node in the graph.
struct JobNode {
    job: MBuildJob,
    status: BuildStatus,
}

/// Load the eval's build_jobs for `derivations`, each paired with its anchor's
/// status. Drives the node + edge mapping (a dep derivation resolves to the
/// build_job the same eval holds for it).
async fn job_nodes_for_derivations(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    derivations: &[DerivationId],
) -> WebResult<HashMap<DerivationId, JobNode>> {
    if derivations.is_empty() {
        return Ok(HashMap::new());
    }

    let jobs = EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation_id))
        .filter(CBuildJob::Derivation.is_in(derivations.to_vec()))
        .all(&state.web_db)
        .await?;
    let anchor_ids: Vec<DerivationBuildId> = jobs.iter().map(|j| j.derivation_build).collect();
    let status_by_anchor: HashMap<DerivationBuildId, BuildStatus> = EDerivationBuild::find()
        .filter(CDerivationBuild::Id.is_in(anchor_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|a| (a.id, a.status))
        .collect();

    Ok(jobs
        .into_iter()
        .map(|job| {
            let status = status_by_anchor
                .get(&job.derivation_build)
                .copied()
                .unwrap_or(BuildStatus::Queued);
            (job.derivation, JobNode { job, status })
        })
        .collect())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DependencyNode {
    pub id: BuildJobId,
    pub name: String,
    pub path: String,
    pub status: String,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DependencyEdge {
    pub source: BuildJobId,
    pub target: BuildJobId,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildGraph {
    pub root: BuildJobId,
    pub nodes: Vec<DependencyNode>,
    pub edges: Vec<DependencyEdge>,
}

// ── Graph BFS helpers ─────────────────────────────────────────────────────────

/// Result of processing one BFS wave in the dependency graph walk.
struct GraphWaveResult {
    nodes: Vec<DependencyNode>,
    edges: Vec<DependencyEdge>,
    /// Build job IDs not yet visited, to be queued for the next wave.
    next_wave: Vec<BuildJobId>,
}

/// Process one BFS wave: fetch build_jobs + derivations for `batch`, resolve
/// dependency edges, and collect unvisited dependents for the next wave.
async fn process_graph_wave(
    state: &Arc<ServerState>,
    batch: &[BuildJobId],
    evaluation_id: EvaluationId,
    visited: &mut HashSet<BuildJobId>,
) -> WebResult<GraphWaveResult> {
    let jobs = EBuildJob::find()
        .filter(CBuildJob::Id.is_in(batch.to_vec()))
        .all(&state.web_db)
        .await?;

    let anchor_ids: Vec<DerivationBuildId> = jobs.iter().map(|j| j.derivation_build).collect();
    let status_by_anchor: HashMap<DerivationBuildId, BuildStatus> = EDerivationBuild::find()
        .filter(CDerivationBuild::Id.is_in(anchor_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|a| (a.id, a.status))
        .collect();

    let drv_ids: Vec<DerivationId> = jobs.iter().map(|j| j.derivation).collect();
    let drv_by_id: HashMap<DerivationId, MDerivation> = EDerivation::find()
        .filter(CDerivation::Id.is_in(drv_ids.clone()))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|d| (d.id, d))
        .collect();

    let mut nodes: Vec<DependencyNode> = Vec::new();
    for job in &jobs {
        if let Some(drv) = drv_by_id.get(&job.derivation) {
            let status = status_by_anchor.get(&job.derivation_build).copied().unwrap_or(BuildStatus::Queued);
            nodes.push(DependencyNode {
                id: job.id,
                name: drv.name.clone(),
                path: drv.drv_path(),
                status: format!("{:?}", status),
                created_at: job.created_at,
                updated_at: job.created_at,
            });
        }
    }

    let dep_rows = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.is_in(drv_ids))
        .all(&state.web_db)
        .await?;

    if dep_rows.is_empty() {
        return Ok(GraphWaveResult {
            nodes,
            edges: vec![],
            next_wave: vec![],
        });
    }

    let dep_drv_ids: Vec<DerivationId> = dep_rows.iter().map(|e| e.dependency).collect();
    let dep_jobs = job_nodes_for_derivations(state, evaluation_id, &dep_drv_ids).await?;
    let job_by_drv: HashMap<DerivationId, BuildJobId> =
        dep_jobs.iter().map(|(drv, jn)| (*drv, jn.job.id)).collect();

    let parent_job_by_drv: HashMap<DerivationId, BuildJobId> =
        jobs.iter().map(|j| (j.derivation, j.id)).collect();

    let mut edges: Vec<DependencyEdge> = Vec::new();
    let mut next_wave: Vec<BuildJobId> = Vec::new();
    for edge in dep_rows {
        let Some(&parent_job_id) = parent_job_by_drv.get(&edge.derivation) else {
            continue;
        };
        let Some(&dep_job_id) = job_by_drv.get(&edge.dependency) else {
            continue;
        };
        edges.push(DependencyEdge {
            source: dep_job_id,
            target: parent_job_id,
        });
        if visited.insert(dep_job_id) {
            next_wave.push(dep_job_id);
        }
    }

    Ok(GraphWaveResult {
        nodes,
        edges,
        next_wave,
    })
}

/// GET /builds/{build}/dependencies - direct dependencies of a single build
pub async fn get_build_dependencies(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<Vec<DependencyNode>>>> {
    authorize_build_opt(&state, build_id, &maybe_user, api_key.as_ref()).await?;

    let build_job = EBuildJob::find_by_id(build_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Build")?;

    let dep_edges = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.eq(build_job.derivation))
        .all(&state.web_db)
        .await?;

    let dep_drv_ids: Vec<DerivationId> = dep_edges.iter().map(|d| d.dependency).collect();

    let mut nodes: Vec<DependencyNode> = Vec::new();
    if !dep_drv_ids.is_empty() {
        let dep_jobs = job_nodes_for_derivations(&state, build_job.evaluation, &dep_drv_ids).await?;
        let dep_drvs = EDerivation::find()
            .filter(CDerivation::Id.is_in(dep_drv_ids))
            .all(&state.web_db)
            .await?;
        let drv_by_id: HashMap<DerivationId, MDerivation> =
            dep_drvs.into_iter().map(|d| (d.id, d)).collect();
        for (drv_id, jn) in dep_jobs {
            if let Some(drv) = drv_by_id.get(&drv_id) {
                nodes.push(DependencyNode {
                    id: jn.job.id,
                    name: drv.name.clone(),
                    path: drv.drv_path(),
                    status: format!("{:?}", jn.status),
                    created_at: jn.job.created_at,
                    updated_at: jn.job.created_at,
                });
            }
        }
    }

    Ok(ok_json(nodes))
}

/// GET /builds/{build}/graph - full transitive dependency graph rooted at a build
pub async fn get_build_graph(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<BuildGraph>>> {
    authorize_build_opt(&state, build_id, &maybe_user, api_key.as_ref()).await?;

    let root_build = EBuildJob::find_by_id(build_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Build")?;
    let evaluation_id = root_build.evaluation;

    let mut visited_builds: HashSet<BuildJobId> = HashSet::new();
    let mut nodes: Vec<DependencyNode> = Vec::new();
    let mut edges: Vec<DependencyEdge> = Vec::new();
    let mut queue: VecDeque<Vec<BuildJobId>> = VecDeque::new();

    visited_builds.insert(build_id);
    queue.push_back(vec![build_id]);

    while let Some(batch) = queue.pop_front() {
        if nodes.len() >= 500 {
            break;
        }
        let wave = process_graph_wave(&state, &batch, evaluation_id, &mut visited_builds).await?;
        nodes.extend(wave.nodes);
        edges.extend(wave.edges);
        if !wave.next_wave.is_empty() {
            queue.push_back(wave.next_wave);
        }
    }

    Ok(ok_json(BuildGraph {
        root: build_id,
        nodes,
        edges,
    }))
}
