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
use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::BuildAccessContext;

const GRAPH_NODE_CAP: usize = 500;

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

struct JobNode {
    job: MBuildJob,
    status: BuildStatus,
}

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
    let shared_build_ids: Vec<DerivationBuildId> =
        jobs.iter().map(|j| j.derivation_build).collect();
    let status_by_shared_build: HashMap<DerivationBuildId, BuildStatus> = EDerivationBuild::find()
        .filter(CDerivationBuild::Id.is_in(shared_build_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|a| (a.id, a.status))
        .collect();

    Ok(jobs
        .into_iter()
        .map(|job| {
            let status = status_by_shared_build
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

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct DependencyGraphNode {
    pub id: DerivationId,
    pub build: Option<BuildJobId>,
    pub name: String,
    pub path: String,
    pub status: String,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct DependencyEdge {
    pub source: DerivationId,
    pub target: DerivationId,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildGraph {
    pub root: DerivationId,
    pub nodes: Vec<DependencyGraphNode>,
    pub edges: Vec<DependencyEdge>,
}

pub async fn dependency_graph<C: ConnectionTrait>(
    db: &C,
    root: &MBuildJob,
) -> WebResult<BuildGraph> {
    let (derivations, edges) = walk_dependencies(db, root.derivation).await?;
    let nodes = graph_nodes(db, root.evaluation, &derivations).await?;

    Ok(BuildGraph {
        root: root.derivation,
        nodes,
        edges,
    })
}

/// The walk follows recorded edges, not this evaluation's builds: an evaluation only names what it
/// walked itself and the direct inputs of that.
async fn walk_dependencies<C: ConnectionTrait>(
    db: &C,
    root: DerivationId,
) -> WebResult<(Vec<DerivationId>, Vec<DependencyEdge>)> {
    let mut reached = vec![root];
    let mut seen = HashSet::from([root]);
    let mut edges = Vec::new();
    let mut frontier = vec![root];

    while !frontier.is_empty() {
        let rows = EDerivationDependency::find()
            .filter(CDerivationDependency::Derivation.is_in(std::mem::take(&mut frontier)))
            .all(db)
            .await?;
        for row in rows {
            if reached.len() < GRAPH_NODE_CAP && seen.insert(row.dependency) {
                reached.push(row.dependency);
                frontier.push(row.dependency);
            }
            if seen.contains(&row.dependency) {
                edges.push(DependencyEdge {
                    source: row.dependency,
                    target: row.derivation,
                });
            }
        }
    }

    Ok((reached, edges))
}

async fn graph_nodes<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    derivations: &[DerivationId],
) -> WebResult<Vec<DependencyGraphNode>> {
    let builds: HashMap<DerivationId, BuildJobId> = EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .filter(CBuildJob::Derivation.is_in(derivations.to_vec()))
        .all(db)
        .await?
        .into_iter()
        .map(|j| (j.derivation, j.id))
        .collect();
    let shared_builds: HashMap<DerivationId, MDerivationBuild> = EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.is_in(derivations.to_vec()))
        .all(db)
        .await?
        .into_iter()
        .map(|b| (b.derivation, b))
        .collect();
    let drv_by_id: HashMap<DerivationId, MDerivation> = EDerivation::find()
        .filter(CDerivation::Id.is_in(derivations.to_vec()))
        .all(db)
        .await?
        .into_iter()
        .map(|d| (d.id, d))
        .collect();

    Ok(derivations
        .iter()
        .filter_map(|id| {
            let drv = drv_by_id.get(id)?;
            let shared_build = shared_builds.get(id);
            Some(DependencyGraphNode {
                id: *id,
                build: builds.get(id).copied(),
                name: drv.name.clone(),
                path: drv.drv_path(),
                status: format!(
                    "{:?}",
                    shared_build.map_or(BuildStatus::Queued, |b| b.status)
                ),
                created_at: shared_build.map_or(drv.created_at, |b| b.created_at),
                updated_at: shared_build.map_or(drv.created_at, |b| b.updated_at),
            })
        })
        .collect())
}

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
        let dep_jobs =
            job_nodes_for_derivations(&state, build_job.evaluation, &dep_drv_ids).await?;
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

pub async fn get_build_graph(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<BuildGraph>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    Ok(ok_json(
        dependency_graph(&state.web_db, &ctx.build_job).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{build_job, derivation, derivation_build, derivation_dependency};
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn drv(id: DerivationId, name: &str) -> derivation::Model {
        derivation::Model {
            id,
            hash: format!("hash{name}"),
            name: name.into(),
            architecture: "x86_64-linux".into(),
            created_at: gradient_types::now(),
            ..Default::default()
        }
    }

    fn shared_build(derivation: DerivationId, status: BuildStatus) -> derivation_build::Model {
        derivation_build::Model {
            id: DerivationBuildId::now_v7(),
            derivation,
            status,
            created_at: gradient_types::now(),
            updated_at: gradient_types::now(),
            ..Default::default()
        }
    }

    fn dep(derivation: DerivationId, dependency: DerivationId) -> derivation_dependency::Model {
        derivation_dependency::Model {
            derivation,
            dependency,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_graph_reaches_dependencies_the_evaluation_has_no_build_for() {
        let evaluation = EvaluationId::now_v7();
        let (root, input, interior) = (
            DerivationId::now_v7(),
            DerivationId::now_v7(),
            DerivationId::now_v7(),
        );
        let root_build = shared_build(root, BuildStatus::Completed);
        let root_job = build_job::Model {
            id: BuildJobId::now_v7(),
            evaluation,
            derivation: root,
            derivation_build: root_build.id,
            created_at: gradient_types::now(),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![dep(root, input)]])
            .append_query_results([vec![dep(input, interior)]])
            .append_query_results([Vec::<derivation_dependency::Model>::new()])
            .append_query_results([vec![root_job.clone()]])
            .append_query_results([vec![
                root_build,
                shared_build(input, BuildStatus::Substituted),
                shared_build(interior, BuildStatus::Substituted),
            ]])
            .append_query_results([vec![
                drv(root, "root"),
                drv(input, "input"),
                drv(interior, "interior"),
            ]])
            .into_connection();

        let graph = dependency_graph(&db, &root_job).await.unwrap();

        let reached: Vec<(DerivationId, Option<BuildJobId>, &str)> = graph
            .nodes
            .iter()
            .map(|n| (n.id, n.build, n.status.as_str()))
            .collect();
        assert_eq!(
            reached,
            [
                (root, Some(root_job.id), "Completed"),
                (input, None, "Substituted"),
                (interior, None, "Substituted"),
            ]
        );
        assert_eq!(
            graph.edges,
            [
                DependencyEdge {
                    source: input,
                    target: root,
                },
                DependencyEdge {
                    source: interior,
                    target: input,
                },
            ]
        );
        assert_eq!(graph.root, root);
    }
}
