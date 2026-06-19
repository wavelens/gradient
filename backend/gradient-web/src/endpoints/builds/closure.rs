/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::evals::EvalAccessContext;
use crate::error::WebResult;
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

use super::BuildAccessContext;

/// Cap on the number of nodes returned in the closure node/edge lists.
/// `total_size_bytes` is always computed over the full closure and stays exact.
const CLOSURE_NODE_CAP: usize = 1000;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ClosureNode {
    pub id: String,
    pub name: String,
    pub path: String,
    pub nar_size: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ClosureEdge {
    pub source: String,
    pub target: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ClosureGraph {
    pub roots: Vec<String>,
    pub total_size_bytes: Option<i64>,
    pub node_count: usize,
    pub edge_count: usize,
    pub truncated: bool,
    pub nodes: Vec<ClosureNode>,
    pub edges: Vec<ClosureEdge>,
}

/// BFS over `derivation_dependency` from `seed_drv_ids`; returns every reachable
/// derivation id (seeds included). Thin wrapper over the shared core helper.
pub async fn derivation_closure_reachable<C: sea_orm::ConnectionTrait>(
    db: &C,
    seed_drv_ids: Vec<DerivationId>,
) -> WebResult<HashSet<DerivationId>> {
    Ok(gradient_db::transitive_closure_reachable(db, &seed_drv_ids).await?)
}

/// Sum coalesced output sizes across `drv_ids`. `Some(total)` when > 0 else `None`.
pub async fn sum_output_sizes<C: sea_orm::ConnectionTrait>(
    db: &C,
    drv_ids: Vec<DerivationId>,
) -> WebResult<Option<i64>> {
    let by_drv = gradient_db::output_sizes_by_drv(db, &drv_ids).await?;
    let total: i64 = by_drv.values().sum();
    Ok(if total > 0 { Some(total) } else { None })
}

/// Build a closure graph seeded at `roots`: full reachable derivation set, exact
/// total size, per-node sizes, and dependency edges restricted to the closure.
pub async fn build_closure_graph<C: sea_orm::ConnectionTrait>(
    db: &C,
    roots: Vec<DerivationId>,
) -> WebResult<ClosureGraph> {
    let closure = derivation_closure_reachable(db, roots.clone()).await?;
    let all_ids: Vec<DerivationId> = closure.iter().cloned().collect();

    let size_by_drv = gradient_db::output_sizes_by_drv(db, &all_ids).await?;
    let total: i64 = size_by_drv.values().sum();
    let total_size_bytes = if total > 0 { Some(total) } else { None };

    let drvs = EDerivation::find()
        .filter(CDerivation::Id.is_in(all_ids.clone()))
        .all(db)
        .await?;

    let mut nodes: Vec<ClosureNode> = drvs
        .into_iter()
        .map(|d| ClosureNode {
            nar_size: size_by_drv.get(&d.id).copied(),
            id: d.id.to_string(),
            name: d.name.clone(),
            path: d.drv_path(),
        })
        .collect();
    // Largest first so a downstream cap keeps the biggest contributors.
    nodes.sort_by_key(|n| std::cmp::Reverse(n.nar_size.unwrap_or(0)));

    let truncated = nodes.len() > CLOSURE_NODE_CAP;
    if truncated {
        nodes.truncate(CLOSURE_NODE_CAP);
    }
    let kept: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();

    let dep_rows = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.is_in(all_ids))
        .all(db)
        .await?;
    let edges: Vec<ClosureEdge> = dep_rows
        .into_iter()
        .map(|e| ClosureEdge {
            source: e.dependency.to_string(),
            target: e.derivation.to_string(),
        })
        .filter(|e| kept.contains(&e.source) && kept.contains(&e.target))
        .collect();

    Ok(ClosureGraph {
        roots: roots.iter().map(|r| r.to_string()).collect(),
        total_size_bytes,
        node_count: nodes.len(),
        edge_count: edges.len(),
        truncated,
        nodes,
        edges,
    })
}

/// GET /builds/{build}/closure - full closure (with sizes) of one build's derivation.
pub async fn get_build_closure(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<ClosureGraph>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let graph = build_closure_graph(&state.web_db, vec![ctx.build_job.derivation]).await?;
    Ok(ok_json(graph))
}

/// GET /evals/{evaluation}/closure - union closure of all entry-point builds.
pub async fn get_eval_closure(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<ClosureGraph>>> {
    let _ctx =
        EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;

    let roots: Vec<DerivationId> = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation_id))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|ep| ep.derivation)
        .collect();

    let graph = build_closure_graph(&state.web_db, roots).await?;
    Ok(ok_json(graph))
}

/// Build a runtime closure graph seeded at the output store-path hashes
/// `seed_hashes`: the transitive `cached_path.references` set with per-node and
/// exact total NAR sizes. Reachability is only as complete as the cached outputs.
pub async fn build_runtime_closure_graph<C: sea_orm::ConnectionTrait>(
    db: &C,
    seed_hashes: Vec<String>,
) -> WebResult<ClosureGraph> {
    let reached = gradient_db::runtime_closure_reachable(db, &seed_hashes).await?;

    let total: i64 = reached.values().filter_map(|r| r.nar_size).sum();
    let total_size_bytes = (total > 0).then_some(total);

    let mut nodes: Vec<ClosureNode> = reached
        .values()
        .map(|r| ClosureNode {
            id: r.hash.clone(),
            name: r.package.clone(),
            path: r.as_store_path().base(),
            nar_size: r.nar_size,
        })
        .collect();
    nodes.sort_by_key(|n| std::cmp::Reverse(n.nar_size.unwrap_or(0)));

    let truncated = nodes.len() > CLOSURE_NODE_CAP;
    if truncated {
        nodes.truncate(CLOSURE_NODE_CAP);
    }
    let kept: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();

    let mut edges: Vec<ClosureEdge> = Vec::new();
    for row in reached.values().filter(|r| kept.contains(&r.hash)) {
        for token in row.references.clone().unwrap_or_default().split_whitespace() {
            if let Some(dep) = gradient_db::parse_reference_hash(token)
                && dep != row.hash
                && kept.contains(&dep)
            {
                edges.push(ClosureEdge {
                    source: dep,
                    target: row.hash.clone(),
                });
            }
        }
    }

    let roots: Vec<String> = seed_hashes
        .into_iter()
        .filter(|h| reached.contains_key(h))
        .collect();

    Ok(ClosureGraph {
        roots,
        total_size_bytes,
        node_count: nodes.len(),
        edge_count: edges.len(),
        truncated,
        nodes,
        edges,
    })
}

/// GET /builds/{build}/runtime-closure - runtime reference closure of a build.
pub async fn get_build_runtime_closure(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<ClosureGraph>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let seeds =
        gradient_db::output_hashes_for_drvs(&state.web_db, &[ctx.build_job.derivation]).await?;
    let graph = build_runtime_closure_graph(&state.web_db, seeds).await?;
    Ok(ok_json(graph))
}

/// GET /evals/{evaluation}/runtime-closure - union runtime closure of entry points.
pub async fn get_eval_runtime_closure(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<ClosureGraph>>> {
    let _ctx =
        EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;

    let roots: Vec<DerivationId> = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation_id))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|ep| ep.derivation)
        .collect();

    let seeds = gradient_db::output_hashes_for_drvs(&state.web_db, &roots).await?;
    let graph = build_runtime_closure_graph(&state.web_db, seeds).await?;
    Ok(ok_json(graph))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{derivation, derivation_dependency, derivation_output};
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn now() -> chrono::NaiveDateTime {
        chrono::Utc::now().naive_utc()
    }

    fn drv(id: DerivationId, name: &str) -> derivation::Model {
        derivation::Model {
            id,
            hash: format!("hash{name}"),
            name: name.into(),
            architecture: "x86_64-linux".into(),
            created_at: now(),
            ..Default::default()
        }
    }

    fn out(derivation: DerivationId, hash: &str, nar_size: Option<i64>) -> derivation_output::Model {
        derivation_output::Model {
            id: DerivationOutputId::now_v7(),
            derivation,
            name: "out".into(),
            hash: hash.into(),
            package: "foo".into(),
            nar_size,
            created_at: now(),
            ..Default::default()
        }
    }

    fn dep(derivation: DerivationId, dependency: DerivationId) -> derivation_dependency::Model {
        derivation_dependency::Model {
            id: DerivationDependencyId::now_v7(),
            derivation,
            dependency,
        }
    }

    // root depends on child; sizes 100 + 40 => total 140, two nodes, one edge.
    #[tokio::test]
    async fn build_closure_graph_sums_and_links() {
        let root = DerivationId::now_v7();
        let child = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // derivation_closure_reachable: wave 1 (root) -> edge root->child
            .append_query_results([vec![dep(root, child)]])
            // wave 2 (child) -> no further edges
            .append_query_results([Vec::<derivation_dependency::Model>::new()])
            // output_sizes_by_drv: outputs for [root, child] (drives both total and per-node)
            .append_query_results([vec![out(root, "r", Some(100)), out(child, "c", Some(40))]])
            // EDerivation::find for nodes
            .append_query_results([vec![drv(root, "root"), drv(child, "child")]])
            // dep_rows for edges
            .append_query_results([vec![dep(root, child)]])
            .into_connection();

        let g = build_closure_graph(&db, vec![root]).await.unwrap();
        assert_eq!(g.total_size_bytes, Some(140));
        assert_eq!(g.node_count, 2);
        assert_eq!(g.edge_count, 1);
        assert!(!g.truncated);
        // Largest first.
        assert_eq!(g.nodes[0].id, root.to_string());
        assert_eq!(g.nodes[0].nar_size, Some(100));
        assert_eq!(
            g.edges[0],
            ClosureEdge {
                source: child.to_string(),
                target: root.to_string(),
            }
        );
    }

    fn cached(hash: &str, nar_size: i64, references: &str) -> gradient_entity::cached_path::Model {
        gradient_entity::cached_path::Model {
            id: CachedPathId::now_v7(),
            hash: hash.into(),
            package: "foo".into(),
            file_hash: Some("sha256:dummy".into()),
            file_size: Some(nar_size / 2),
            nar_size: Some(nar_size),
            references: (!references.is_empty()).then(|| references.to_string()),
            created_at: now(),
            ..Default::default()
        }
    }

    // root -refs-> child; sizes 100 + 40 => total 140, two nodes, one edge.
    #[tokio::test]
    async fn runtime_closure_graph_sums_and_links() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![cached("root", 100, "child-foo")]])
            .append_query_results([vec![cached("child", 40, "")]])
            .into_connection();

        let g = build_runtime_closure_graph(&db, vec!["root".into()])
            .await
            .unwrap();
        assert_eq!(g.total_size_bytes, Some(140));
        assert_eq!(g.node_count, 2);
        assert_eq!(g.edge_count, 1);
        assert_eq!(g.roots, vec!["root".to_string()]);
        assert_eq!(g.nodes[0].id, "root");
        assert_eq!(
            g.edges[0],
            ClosureEdge {
                source: "child".into(),
                target: "root".into(),
            }
        );
    }
}
