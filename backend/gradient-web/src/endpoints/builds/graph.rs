/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::WebResult;
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_entity::derivation_dependency::EdgeKind;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::BuildAccessContext;

const GRAPH_NODE_CAP: usize = 500;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct DependencyGraphNode {
    pub id: DerivationId,
    pub build: Option<BuildJobId>,
    pub name: String,
    pub path: String,
    pub status: Option<String>,
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
            .filter(CDerivationDependency::Kind.is_in([EdgeKind::Buildtime, EdgeKind::Both]))
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
                status: shared_build.map(|b| format!("{:?}", b.status)),
                created_at: shared_build.map_or(drv.created_at, |b| b.created_at),
                updated_at: shared_build.map_or(drv.created_at, |b| b.updated_at),
            })
        })
        .collect())
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
    use gradient_entity::build::BuildStatus;
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
            ]])
            .append_query_results([vec![
                drv(root, "root"),
                drv(input, "input"),
                drv(interior, "interior"),
            ]])
            .into_connection();

        let graph = dependency_graph(&db, &root_job).await.unwrap();

        let reached: Vec<(DerivationId, Option<BuildJobId>, Option<&str>)> = graph
            .nodes
            .iter()
            .map(|n| (n.id, n.build, n.status.as_deref()))
            .collect();
        assert_eq!(
            reached,
            [
                (root, Some(root_job.id), Some("Completed")),
                (input, None, Some("Substituted")),
                (interior, None, None),
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

    #[tokio::test]
    async fn the_walk_follows_build_inputs_only() {
        let root_job = build_job::Model {
            derivation: DerivationId::now_v7(),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<derivation_dependency::Model>::new()])
            .append_query_results([Vec::<build_job::Model>::new()])
            .append_query_results([Vec::<derivation_build::Model>::new()])
            .append_query_results([Vec::<derivation::Model>::new()])
            .into_connection();

        dependency_graph(&db, &root_job).await.unwrap();

        let log = db.into_transaction_log();
        let walk = &log[0].statements()[0];
        let values = &walk.values.as_ref().unwrap().0;
        assert!(walk.sql.contains(r#""kind" IN"#), "{}", walk.sql);
        assert!(values.contains(&sea_orm::Value::SmallInt(Some(0))));
        assert!(values.contains(&sea_orm::Value::SmallInt(Some(2))));
        assert!(!values.contains(&sea_orm::Value::SmallInt(Some(1))));
    }
}
