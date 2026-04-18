/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use core::types::*;
use sea_orm::EntityTrait;
use sea_orm::{ColumnTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use uuid::Uuid;

use super::BuildAccessContext;

// ── Dependency graph helpers ──────────────────────────────────────────────────

pub(super) fn extract_drv_name(path: &str) -> String {
    let filename = path.split('/').next_back().unwrap_or(path);
    // Strip the nix store hash prefix (e.g. "abc123xyz-name.drv" → "name")
    let without_hash = filename.split_once('-').map(|x| x.1).unwrap_or(filename);
    without_hash.trim_end_matches(".drv").to_string()
}

pub(super) async fn authorize_build_opt(
    state: &Arc<ServerState>,
    maybe_user: &Option<MUser>,
    build_id: Uuid,
) -> WebResult<()> {
    BuildAccessContext::load(state, build_id, maybe_user)
        .await
        .map(|_| ())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DependencyNode {
    pub id: Uuid,
    pub name: String,
    pub path: String,
    pub status: String,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DependencyEdge {
    pub source: Uuid,
    pub target: Uuid,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildGraph {
    pub root: Uuid,
    pub nodes: Vec<DependencyNode>,
    pub edges: Vec<DependencyEdge>,
}

// ── Graph BFS helpers ─────────────────────────────────────────────────────────

/// Result of processing one BFS wave in the dependency graph walk.
struct GraphWaveResult {
    nodes: Vec<DependencyNode>,
    edges: Vec<DependencyEdge>,
    /// Build IDs not yet visited, to be queued for the next wave.
    next_wave: Vec<Uuid>,
}

/// Process one BFS wave: fetch builds+derivations for `batch`, resolve
/// dependency edges, and collect unvisited dependents for the next wave.
async fn process_graph_wave(
    state: &Arc<ServerState>,
    batch: &[Uuid],
    evaluation_id: Uuid,
    visited: &mut HashSet<Uuid>,
) -> WebResult<GraphWaveResult> {
    let builds = EBuild::find()
        .filter(CBuild::Id.is_in(batch.to_vec()))
        .all(&state.db)
        .await?;

    let drv_ids: Vec<Uuid> = builds.iter().map(|b| b.derivation).collect();
    let drv_by_id: HashMap<Uuid, MDerivation> = EDerivation::find()
        .filter(CDerivation::Id.is_in(drv_ids.clone()))
        .all(&state.db)
        .await?
        .into_iter()
        .map(|d| (d.id, d))
        .collect();

    let mut nodes: Vec<DependencyNode> = Vec::new();
    for build in &builds {
        if let Some(drv) = drv_by_id.get(&build.derivation) {
            nodes.push(DependencyNode {
                id: build.id,
                name: extract_drv_name(&drv.derivation_path),
                path: drv.derivation_path.clone(),
                status: format!("{:?}", build.status),
                created_at: build.created_at,
                updated_at: build.updated_at,
            });
        }
    }

    let dep_rows = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.is_in(drv_ids))
        .all(&state.db)
        .await?;

    if dep_rows.is_empty() {
        return Ok(GraphWaveResult {
            nodes,
            edges: vec![],
            next_wave: vec![],
        });
    }

    let dep_drv_ids: Vec<Uuid> = dep_rows.iter().map(|e| e.dependency).collect();
    let build_by_drv: HashMap<Uuid, Uuid> = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .filter(CBuild::Derivation.is_in(dep_drv_ids))
        .all(&state.db)
        .await?
        .iter()
        .map(|b| (b.derivation, b.id))
        .collect();

    let parent_build_by_drv: HashMap<Uuid, Uuid> =
        builds.iter().map(|b| (b.derivation, b.id)).collect();

    let mut edges: Vec<DependencyEdge> = Vec::new();
    let mut next_wave: Vec<Uuid> = Vec::new();
    for edge in dep_rows {
        let Some(&parent_build_id) = parent_build_by_drv.get(&edge.derivation) else {
            continue;
        };
        let Some(&dep_build_id) = build_by_drv.get(&edge.dependency) else {
            continue;
        };
        edges.push(DependencyEdge {
            source: dep_build_id,
            target: parent_build_id,
        });
        if visited.insert(dep_build_id) {
            next_wave.push(dep_build_id);
        }
    }

    Ok(GraphWaveResult {
        nodes,
        edges,
        next_wave,
    })
}

/// GET /builds/{build}/dependencies — direct dependencies of a single build
pub async fn get_build_dependencies(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<Vec<DependencyNode>>>> {
    authorize_build_opt(&state, &maybe_user, build_id).await?;

    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let dep_edges = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.eq(build.derivation))
        .all(&state.db)
        .await?;

    let dep_drv_ids: Vec<Uuid> = dep_edges.iter().map(|d| d.dependency).collect();

    let mut nodes: Vec<DependencyNode> = Vec::new();
    if !dep_drv_ids.is_empty() {
        let dep_builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(build.evaluation))
            .filter(CBuild::Derivation.is_in(dep_drv_ids.clone()))
            .all(&state.db)
            .await?;
        let dep_drvs = EDerivation::find()
            .filter(CDerivation::Id.is_in(dep_drv_ids))
            .all(&state.db)
            .await?;
        let drv_by_id: HashMap<Uuid, MDerivation> =
            dep_drvs.into_iter().map(|d| (d.id, d)).collect();
        for b in dep_builds {
            if let Some(drv) = drv_by_id.get(&b.derivation) {
                nodes.push(DependencyNode {
                    id: b.id,
                    name: extract_drv_name(&drv.derivation_path),
                    path: drv.derivation_path.clone(),
                    status: format!("{:?}", b.status),
                    created_at: b.created_at,
                    updated_at: b.updated_at,
                });
            }
        }
    }

    Ok(Json(BaseResponse {
        error: false,
        message: nodes,
    }))
}

/// GET /builds/{build}/graph — full transitive dependency graph rooted at a build
pub async fn get_build_graph(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<BuildGraph>>> {
    authorize_build_opt(&state, &maybe_user, build_id).await?;

    let root_build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;
    let evaluation_id = root_build.evaluation;

    let mut visited_builds: HashSet<Uuid> = HashSet::new();
    let mut nodes: Vec<DependencyNode> = Vec::new();
    let mut edges: Vec<DependencyEdge> = Vec::new();
    let mut queue: VecDeque<Vec<Uuid>> = VecDeque::new();

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

    Ok(Json(BaseResponse {
        error: false,
        message: BuildGraph {
            root: build_id,
            nodes,
            edges,
        },
    }))
}
