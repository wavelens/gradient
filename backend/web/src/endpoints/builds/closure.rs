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
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::BuildAccessContext;

/// Cap on the number of nodes returned in the closure node/edge lists.
/// `total_size_bytes` is always computed over the full closure and stays exact.
const CLOSURE_NODE_CAP: usize = 1000;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ClosureNode {
    pub id: DerivationId,
    pub name: String,
    pub path: String,
    pub nar_size: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ClosureEdge {
    pub source: DerivationId,
    pub target: DerivationId,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ClosureGraph {
    pub roots: Vec<DerivationId>,
    pub total_size_bytes: Option<i64>,
    pub node_count: usize,
    pub edge_count: usize,
    pub truncated: bool,
    pub nodes: Vec<ClosureNode>,
    pub edges: Vec<ClosureEdge>,
}

/// BFS over `derivation_dependency` from `seed_drv_ids`; returns every reachable
/// derivation id (seeds included).
pub async fn derivation_closure_reachable<C: sea_orm::ConnectionTrait>(
    db: &C,
    seed_drv_ids: Vec<DerivationId>,
) -> WebResult<HashSet<DerivationId>> {
    let mut visited: HashSet<DerivationId> = seed_drv_ids.iter().cloned().collect();
    let mut frontier = seed_drv_ids;

    while !frontier.is_empty() {
        let edges = EDerivationDependency::find()
            .filter(CDerivationDependency::Derivation.is_in(frontier.clone()))
            .all(db)
            .await?;
        frontier.clear();
        for edge in edges {
            if visited.insert(edge.dependency) {
                frontier.push(edge.dependency);
            }
        }
    }

    Ok(visited)
}

/// Map each derivation id to its coalesced output NAR size
/// (`derivation_output.nar_size`, else matching `cached_path.nar_size`).
async fn output_sizes_by_drv<C: sea_orm::ConnectionTrait>(
    db: &C,
    drv_ids: Vec<DerivationId>,
) -> WebResult<HashMap<DerivationId, i64>> {
    if drv_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.is_in(drv_ids))
        .all(db)
        .await?;

    let missing_hashes: Vec<String> = outputs
        .iter()
        .filter(|o| o.nar_size.is_none())
        .map(|o| o.hash.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let cached_size_by_hash: HashMap<String, i64> = if missing_hashes.is_empty() {
        HashMap::new()
    } else {
        ECachedPath::find()
            .filter(CCachedPath::Hash.is_in(missing_hashes))
            .all(db)
            .await?
            .into_iter()
            .filter_map(|cp| cp.nar_size.map(|n| (cp.hash, n)))
            .collect()
    };

    let mut by_drv: HashMap<DerivationId, i64> = HashMap::new();
    for o in outputs {
        if let Some(size) = o
            .nar_size
            .or_else(|| cached_size_by_hash.get(&o.hash).copied())
        {
            *by_drv.entry(o.derivation).or_insert(0) += size;
        }
    }
    Ok(by_drv)
}

/// Sum coalesced output sizes across `drv_ids`. `Some(total)` when > 0 else `None`.
pub async fn sum_output_sizes<C: sea_orm::ConnectionTrait>(
    db: &C,
    drv_ids: Vec<DerivationId>,
) -> WebResult<Option<i64>> {
    let by_drv = output_sizes_by_drv(db, drv_ids).await?;
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

    let total_size_bytes = sum_output_sizes(db, all_ids.clone()).await?;
    let size_by_drv = output_sizes_by_drv(db, all_ids.clone()).await?;

    let drvs = EDerivation::find()
        .filter(CDerivation::Id.is_in(all_ids.clone()))
        .all(db)
        .await?;

    let mut nodes: Vec<ClosureNode> = drvs
        .into_iter()
        .map(|d| ClosureNode {
            nar_size: size_by_drv.get(&d.id).copied(),
            id: d.id,
            name: d.name.clone(),
            path: d.store_path(),
        })
        .collect();
    // Largest first so a downstream cap keeps the biggest contributors.
    nodes.sort_by_key(|n| std::cmp::Reverse(n.nar_size.unwrap_or(0)));

    let truncated = nodes.len() > CLOSURE_NODE_CAP;
    if truncated {
        nodes.truncate(CLOSURE_NODE_CAP);
    }
    let kept: HashSet<DerivationId> = nodes.iter().map(|n| n.id).collect();

    let dep_rows = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.is_in(all_ids))
        .all(db)
        .await?;
    let edges: Vec<ClosureEdge> = dep_rows
        .into_iter()
        .filter(|e| kept.contains(&e.derivation) && kept.contains(&e.dependency))
        .map(|e| ClosureEdge {
            source: e.dependency,
            target: e.derivation,
        })
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

/// GET /builds/{build}/closure - full closure (with sizes) of one build's derivation.
pub async fn get_build_closure(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildId>,
) -> WebResult<Json<BaseResponse<ClosureGraph>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let graph = build_closure_graph(&state.web_db, vec![ctx.build.derivation]).await?;
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

    let ep_build_ids: Vec<BuildId> = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation_id))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|ep| ep.build)
        .collect();

    let roots: Vec<DerivationId> = if ep_build_ids.is_empty() {
        vec![]
    } else {
        EBuild::find()
            .filter(CBuild::Id.is_in(ep_build_ids))
            .all(&state.web_db)
            .await?
            .into_iter()
            .map(|b| b.derivation)
            .collect()
    };

    let graph = build_closure_graph(&state.web_db, roots).await?;
    Ok(ok_json(graph))
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity::{derivation, derivation_dependency, derivation_output};
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn now() -> chrono::NaiveDateTime {
        chrono::Utc::now().naive_utc()
    }

    fn drv(id: DerivationId, name: &str) -> derivation::Model {
        derivation::Model {
            id,
            organization: OrganizationId::now_v7(),
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
            ca: None,
            nar_size,
            is_cached: false,
            cached_path: None,
            created_at: now(),
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
            // sum_output_sizes (total): outputs for [root, child]
            .append_query_results([vec![out(root, "r", Some(100)), out(child, "c", Some(40))]])
            // output_sizes_by_drv (per-node): outputs again
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
        assert_eq!(g.nodes[0].id, root);
        assert_eq!(g.nodes[0].nar_size, Some(100));
        assert_eq!(
            g.edges[0],
            ClosureEdge {
                source: child,
                target: root
            }
        );
    }
}
