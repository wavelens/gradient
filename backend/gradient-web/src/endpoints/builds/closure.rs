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
use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::BuildAccessContext;

/// `total_size_bytes` is always computed over the full closure and is exact.
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

pub async fn derivation_closure_reachable<C>(
    db: &C,
    seed_drv_ids: Vec<DerivationId>,
) -> WebResult<HashSet<DerivationId>>
where
    C: sea_orm::ConnectionTrait
        + sea_orm::TransactionTrait<Transaction = sea_orm::DatabaseTransaction>,
{
    Ok(gradient_db::graph::closure::transitive_closure_reachable(db, &seed_drv_ids).await?)
}

pub async fn sum_output_sizes<C: sea_orm::ConnectionTrait>(
    db: &C,
    drv_ids: Vec<DerivationId>,
) -> WebResult<Option<i64>> {
    let by_drv = gradient_db::graph::closure::output_sizes_by_drv(db, &drv_ids).await?;
    let total: i64 = by_drv.values().sum();
    Ok(if total > 0 { Some(total) } else { None })
}

pub async fn build_closure_graph<C>(db: &C, roots: Vec<DerivationId>) -> WebResult<ClosureGraph>
where
    C: sea_orm::ConnectionTrait
        + sea_orm::TransactionTrait<Transaction = sea_orm::DatabaseTransaction>,
{
    let closure = derivation_closure_reachable(db, roots.clone()).await?;
    let all_ids: Vec<DerivationId> = closure.iter().cloned().collect();

    let size_by_drv = gradient_db::graph::closure::output_sizes_by_drv(db, &all_ids).await?;
    let total: i64 = size_by_drv.values().sum();
    let total_size_bytes = if total > 0 { Some(total) } else { None };

    let nodes: Vec<ClosureNode> = EDerivation::find()
        .filter(CDerivation::Id.is_in(all_ids.clone()))
        .all(db)
        .await?
        .into_iter()
        .map(|d| ClosureNode {
            nar_size: size_by_drv.get(&d.id).copied(),
            id: d.id.to_string(),
            name: d.name.clone(),
            path: d.drv_path(),
        })
        .collect();

    let edges: Vec<ClosureEdge> = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.is_in(all_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|e| ClosureEdge {
            source: e.dependency.to_string(),
            target: e.derivation.to_string(),
        })
        .collect();

    let roots = roots.iter().map(|r| r.to_string()).collect();
    Ok(closure_graph(roots, total_size_bytes, nodes, edges))
}

fn closure_graph(
    roots: Vec<String>,
    total_size_bytes: Option<i64>,
    nodes: Vec<ClosureNode>,
    edges: Vec<ClosureEdge>,
) -> ClosureGraph {
    let truncated = nodes.len() > CLOSURE_NODE_CAP;
    let rank = heaviest_rooted(&roots, &nodes, &edges, CLOSURE_NODE_CAP);

    let mut nodes: Vec<ClosureNode> = nodes
        .into_iter()
        .filter(|n| rank.contains_key(&n.id))
        .collect();
    nodes.sort_by_key(|n| rank[&n.id]);
    let edges: Vec<ClosureEdge> = edges
        .into_iter()
        .filter(|e| rank.contains_key(&e.source) && rank.contains_key(&e.target))
        .collect();

    ClosureGraph {
        roots,
        total_size_bytes,
        node_count: nodes.len(),
        edge_count: edges.len(),
        truncated,
        nodes,
        edges,
    }
}

/// A node is never kept without the node that reached it, so every kept node still leads to a root.
fn heaviest_rooted(
    roots: &[String],
    nodes: &[ClosureNode],
    edges: &[ClosureEdge],
    cap: usize,
) -> HashMap<String, usize> {
    let order = spanning_order(roots, nodes, edges);

    let mut subtree: HashMap<&str, i64> = nodes
        .iter()
        .map(|n| (n.id.as_str(), n.nar_size.unwrap_or(0)))
        .collect();
    for &(node, parent) in order.iter().rev() {
        if let Some(parent) = parent {
            let size = subtree[node];
            *subtree.entry(parent).or_default() += size;
        }
    }

    let mut ranked: Vec<(usize, &str)> = order
        .iter()
        .enumerate()
        .map(|(index, &(node, _))| (index, node))
        .collect();
    ranked.sort_by_key(|&(index, node)| (std::cmp::Reverse(subtree[node]), index));

    ranked
        .into_iter()
        .take(cap)
        .enumerate()
        .map(|(rank, (_, node))| (node.to_owned(), rank))
        .collect()
}

fn spanning_order<'a>(
    roots: &'a [String],
    nodes: &'a [ClosureNode],
    edges: &'a [ClosureEdge],
) -> Vec<(&'a str, Option<&'a str>)> {
    let known: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut dependencies: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in edges {
        dependencies
            .entry(e.target.as_str())
            .or_default()
            .push(e.source.as_str());
    }

    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<(&str, Option<&str>)> = roots
        .iter()
        .map(String::as_str)
        .filter(|r| known.contains(r) && seen.insert(r))
        .map(|r| (r, None))
        .collect();
    let mut unreached = nodes.iter().map(|n| n.id.as_str());
    let mut order = Vec::with_capacity(nodes.len());
    loop {
        while let Some((node, parent)) = queue.pop_front() {
            order.push((node, parent));
            for &dep in dependencies.get(node).into_iter().flatten() {
                if known.contains(dep) && seen.insert(dep) {
                    queue.push_back((dep, Some(node)));
                }
            }
        }
        match unreached.find(|id| seen.insert(id)) {
            Some(id) => queue.push_back((id, None)),
            None => break,
        }
    }

    order
}

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

pub async fn build_runtime_closure_graph<C>(
    db: &C,
    seed_hashes: Vec<String>,
) -> WebResult<ClosureGraph>
where
    C: sea_orm::ConnectionTrait
        + sea_orm::TransactionTrait<Transaction = sea_orm::DatabaseTransaction>,
{
    let reached =
        gradient_db::graph::runtime_closure::runtime_closure_reachable(db, &seed_hashes).await?;

    let total: i64 = reached.values().filter_map(|r| r.nar_size).sum();
    let total_size_bytes = (total > 0).then_some(total);

    let nodes: Vec<ClosureNode> = reached
        .values()
        .map(|r| ClosureNode {
            id: r.hash.clone(),
            name: r.package.clone(),
            path: r.as_store_path().base(),
            nar_size: r.nar_size,
        })
        .collect();

    let edges: Vec<ClosureEdge> = reached
        .values()
        .flat_map(|r| {
            r.references
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(gradient_db::graph::runtime_closure::parse_reference_hash)
                .filter(|dep| *dep != r.hash && reached.contains_key(dep))
                .map(|dep| ClosureEdge {
                    source: dep,
                    target: r.hash.clone(),
                })
        })
        .collect();

    let roots: Vec<String> = seed_hashes
        .into_iter()
        .filter(|h| reached.contains_key(h))
        .collect();

    Ok(closure_graph(roots, total_size_bytes, nodes, edges))
}

pub async fn get_build_runtime_closure(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<ClosureGraph>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let seeds = gradient_db::graph::runtime_closure::output_hashes_for_drvs(
        &state.web_db,
        &[ctx.build_job.derivation],
    )
    .await?;
    let graph = build_runtime_closure_graph(&state.web_db, seeds).await?;
    Ok(ok_json(graph))
}

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

    let seeds =
        gradient_db::graph::runtime_closure::output_hashes_for_drvs(&state.web_db, &roots).await?;
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

    fn out(
        derivation: DerivationId,
        hash: &str,
        nar_size: Option<i64>,
    ) -> derivation_output::Model {
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
            derivation,
            dependency,
            ..Default::default()
        }
    }

    fn node(derivation: DerivationId) -> derivation_dependency::Model {
        dep(derivation, derivation)
    }

    #[tokio::test]
    async fn build_closure_graph_sums_and_links() {
        let root = DerivationId::now_v7();
        let child = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![node(root), node(child)]])
            .append_query_results([vec![out(root, "r", Some(100)), out(child, "c", Some(40))]])
            .append_query_results([vec![drv(root, "root"), drv(child, "child")]])
            .append_query_results([vec![dep(root, child)]])
            .into_connection();

        let g = build_closure_graph(&db, vec![root]).await.unwrap();
        assert_eq!(g.total_size_bytes, Some(140));
        assert_eq!(g.node_count, 2);
        assert_eq!(g.edge_count, 1);
        assert!(!g.truncated);
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

    fn sized(id: &str, nar_size: i64) -> ClosureNode {
        ClosureNode {
            id: id.into(),
            name: id.into(),
            path: id.into(),
            nar_size: Some(nar_size),
        }
    }

    fn needs(target: &str, source: &str) -> ClosureEdge {
        ClosureEdge {
            source: source.into(),
            target: target.into(),
        }
    }

    #[test]
    fn a_capped_closure_keeps_its_small_root_and_the_heaviest_paths_down_to_it() {
        let nodes = vec![
            sized("root", 1),
            sized("light", 10),
            sized("heavy", 100),
            sized("deep", 50),
        ];
        let edges = vec![
            needs("root", "light"),
            needs("root", "heavy"),
            needs("heavy", "deep"),
        ];

        let rank = heaviest_rooted(&["root".to_string()], &nodes, &edges, 3);

        let mut kept: Vec<&str> = rank.keys().map(String::as_str).collect();
        kept.sort_unstable();
        assert_eq!(kept, ["deep", "heavy", "root"]);
        assert_eq!(rank["root"], 0);
    }
}
