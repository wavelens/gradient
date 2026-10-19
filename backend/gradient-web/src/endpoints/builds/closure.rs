/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::evals::EvalAccessContext;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::store_paths::original_text;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::BuildAccessContext;

const CLOSURE_NODE_CAP: usize = 1000;

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct ClosureNode {
    pub id: String,
    pub name: String,
    pub path: String,
    pub nar_size: Option<i64>,
}

impl ClosureNode {
    fn of_derivation(derivation: MDerivation, nar_size: Option<i64>) -> Self {
        Self {
            id: derivation.id.to_string(),
            path: derivation.drv_path(),
            name: derivation.name,
            nar_size,
        }
    }
}

impl From<&MCachedPath> for ClosureNode {
    fn from(path: &MCachedPath) -> Self {
        Self {
            id: path.hash.clone(),
            name: path.package.clone(),
            path: path.as_store_path().base(),
            nar_size: path.nar_size,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct ClosureEdge {
    pub source: String,
    pub target: String,
}

impl From<MDerivationDependency> for ClosureEdge {
    fn from(edge: MDerivationDependency) -> Self {
        Self {
            source: edge.dependency.to_string(),
            target: edge.derivation.to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct ClosureGraph {
    pub roots: Vec<String>,
    pub total_size_bytes: Option<i64>,
    pub node_count: usize,
    pub edge_count: usize,
    pub truncated: bool,
    pub nodes: Vec<ClosureNode>,
    pub edges: Vec<ClosureEdge>,
}

impl ClosureGraph {
    fn with_original_paths(mut self, mapper: &StorePathMapper) -> Self {
        for node in &mut self.nodes {
            node.path = original_text(mapper, std::mem::take(&mut node.path));
        }
        self
    }

    fn with_original_hashes(mut self, mapper: &StorePathMapper) -> Self {
        let original = |hash: &mut String| *hash = original_text(mapper, std::mem::take(hash));
        self.roots.iter_mut().for_each(original);
        for node in &mut self.nodes {
            original(&mut node.id);
        }
        for edge in &mut self.edges {
            original(&mut edge.source);
            original(&mut edge.target);
        }
        self.with_original_paths(mapper)
    }
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

    let nodes: Vec<ClosureNode> = gradient_db::derivations::with_ids(db, &all_ids)
        .await?
        .into_iter()
        .map(|d| {
            let nar_size = size_by_drv.get(&d.id).copied();
            ClosureNode::of_derivation(d, nar_size)
        })
        .collect();

    let edges: Vec<ClosureEdge> =
        gradient_db::derivation_dependencies::of_derivations(db, &all_ids)
            .await?
            .into_iter()
            .map(ClosureEdge::from)
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

#[utoipa::path(
    get,
    path = "/builds/{build}/closure",
    operation_id = "getBuildClosure",
    tag = "builds",
    summary = "Get closure size graph",
    description = "Returns the full dependency closure of the build's derivation with per-node\nuncompressed NAR sizes and an exact `total_size_bytes` over the whole closure.\nThe `nodes`/`edges` lists are capped with `truncated` set when the cap is hit:\nthe heaviest subtrees are kept, each still linked to a root. `total_size_bytes`\nstays exact regardless. Intended\nfor the closure Sankey view and size-trimming tooling.\n\n**Access:** Same as the dependency graph endpoint.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Closure size graph", body = ClosureGraph), WebError)
)]
pub async fn get_build_closure(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<ClosureGraph>>> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let mapper = state.store_path_mapper(MappingContext::Project(ctx.project.id));
    let graph = build_closure_graph(&state.web_db, vec![ctx.build_job.derivation]).await?;
    Ok(ok_json(graph.with_original_paths(&mapper)))
}

#[utoipa::path(
    get,
    path = "/evals/{evaluation}/closure",
    operation_id = "getEvalClosure",
    tag = "evals",
    summary = "Get closure size graph for an evaluation",
    description = "Returns the union dependency closure of all the evaluation's entry-point\nbuilds with per-node uncompressed NAR sizes and an exact `total_size_bytes`.\nSame shape as `/builds/{build}/closure`.",
    params(("evaluation" = EvaluationId, Path)),
    responses((status = OK, description = "Closure size graph", body = ClosureGraph), WebError)
)]
pub async fn get_eval_closure(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<ClosureGraph>>> {
    let ctx = EvalAccessContext::load(&state.web_db, evaluation_id, &maybe_user, api_key.as_ref())
        .await?;

    let roots: Vec<DerivationId> =
        gradient_db::entry_points::of_evaluation(&state.web_db, evaluation_id)
            .await?
            .into_iter()
            .map(|ep| ep.derivation)
            .collect();

    let mapper = state.store_path_mapper(MappingContext::Project(ctx.project_id));
    let graph = build_closure_graph(&state.web_db, roots).await?;
    Ok(ok_json(graph.with_original_paths(&mapper)))
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

    let nodes: Vec<ClosureNode> = reached.values().map(ClosureNode::from).collect();

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

#[utoipa::path(
    get,
    path = "/builds/{build}/runtime-closure",
    operation_id = "getBuildRuntimeClosure",
    tag = "builds",
    summary = "Get runtime closure size graph",
    description = "Like `/builds/{build}/closure` but walks runtime store-path references\n(narinfo `References:`) from the build's outputs instead of build-time\ndependencies. Only as complete as the cached outputs; node ids are\nstore-path hashes. Same `ClosureGraph` shape.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Runtime closure size graph", body = ClosureGraph), WebError)
)]
pub async fn get_build_runtime_closure(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<ClosureGraph>>> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let seeds = gradient_db::graph::runtime_closure::output_hashes_for_drvs(
        &state.web_db,
        &[ctx.build_job.derivation],
    )
    .await?;

    let mapper = state.store_path_mapper(MappingContext::Project(ctx.project.id));
    let graph = build_runtime_closure_graph(&state.web_db, seeds).await?;
    Ok(ok_json(graph.with_original_hashes(&mapper)))
}

#[utoipa::path(
    get,
    path = "/evals/{evaluation}/runtime-closure",
    operation_id = "getEvalRuntimeClosure",
    tag = "evals",
    summary = "Get runtime closure size graph for an evaluation",
    description = "Union runtime reference closure of all the evaluation's entry-point build\noutputs. Runtime counterpart of `/evals/{evaluation}/closure`.",
    params(("evaluation" = EvaluationId, Path)),
    responses((status = OK, description = "Runtime closure size graph", body = ClosureGraph), WebError)
)]
pub async fn get_eval_runtime_closure(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<ClosureGraph>>> {
    let ctx = EvalAccessContext::load(&state.web_db, evaluation_id, &maybe_user, api_key.as_ref())
        .await?;

    let roots: Vec<DerivationId> =
        gradient_db::entry_points::of_evaluation(&state.web_db, evaluation_id)
            .await?
            .into_iter()
            .map(|ep| ep.derivation)
            .collect();

    let seeds =
        gradient_db::graph::runtime_closure::output_hashes_for_drvs(&state.web_db, &roots).await?;
    let mapper = state.store_path_mapper(MappingContext::Project(ctx.project_id));
    let graph = build_runtime_closure_graph(&state.web_db, seeds).await?;
    Ok(ok_json(graph.with_original_hashes(&mapper)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use gradient_fixtures::rows::{derivation_dependency, derivation_named, derivation_output};

    #[tokio::test]
    async fn build_closure_graph_sums_and_links() {
        let root = DerivationId::now_v7();
        let child = DerivationId::now_v7();
        let db = mock_db()
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![
                derivation_dependency(root, root),
                derivation_dependency(child, child),
            ]])
            .append_query_results([vec![
                derivation_output(root, "r", Some(100)),
                derivation_output(child, "c", Some(40)),
            ]])
            .append_query_results([vec![
                derivation_named(root, "root"),
                derivation_named(child, "child"),
            ]])
            .append_query_results([vec![derivation_dependency(root, child)]])
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
