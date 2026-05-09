/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, OrgAccess, load_org};
use crate::authorization::MaybeUser;
use crate::error::WebResult;
use crate::helpers::{OptionExt, ok_json};
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectMetricPoint {
    pub evaluation_id: EvaluationId,
    pub created_at: chrono::NaiveDateTime,
    pub build_time_total_ms: i64,
    pub eval_time_ms: i64,
    pub output_size_bytes: Option<i64>,
    pub closure_size_bytes: Option<i64>,
    pub dependencies_count: i64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectMetricsResponse {
    pub keep_evaluations: i32,
    pub points: Vec<ProjectMetricPoint>,
}

// ── Shared metric helpers ─────────────────────────────────────────────────────

/// BFS over `derivation_dependency` starting from `seed_drv_ids`.
///
/// Returns all reachable derivation UUIDs (including the seeds themselves).
async fn derivation_closure_reachable<C: sea_orm::ConnectionTrait>(
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

/// Sum uncompressed NAR size of derivation outputs for `drv_ids`.
///
/// `derivation_output.nar_size` is populated only when the worker actually
/// built the output. For substituted builds it's `None`, but the size lives
/// on the matching `cached_path` row (joined by hash). This helper coalesces
/// the two so that completed and substituted outputs both contribute.
///
/// Returns `Some(total)` when the total is > 0, `None` otherwise.
async fn sum_output_sizes<C: sea_orm::ConnectionTrait>(
    db: &C,
    drv_ids: Vec<DerivationId>,
) -> WebResult<Option<i64>> {
    if drv_ids.is_empty() {
        return Ok(None);
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

    let cached_size_by_hash: std::collections::HashMap<String, i64> = if missing_hashes.is_empty() {
        std::collections::HashMap::new()
    } else {
        ECachedPath::find()
            .filter(CCachedPath::Hash.is_in(missing_hashes))
            .all(db)
            .await?
            .into_iter()
            .filter_map(|cp| cp.nar_size.map(|n| (cp.hash, n)))
            .collect()
    };

    let total: i64 = outputs
        .iter()
        .filter_map(|o| {
            o.nar_size
                .or_else(|| cached_size_by_hash.get(&o.hash).copied())
        })
        .sum();
    Ok(if total > 0 { Some(total) } else { None })
}

// ── Endpoints ─────────────────────────────────────────────────────────────────

pub async fn get_project_metrics(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<ProjectMetricsResponse>>> {
    let organization = load_org(
        &state.0,
        Caller::from_option(&maybe_user),
        organization,
        OrgAccess::Readable { label: "Project" },
    )
    .await?;

    let project = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(project))
        .one(&state.web_db)
        .await?
        .or_not_found("Project")?;

    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(CEvaluation::Status.eq(entity::evaluation::EvaluationStatus::Completed))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(project.keep_evaluations as u64)
        .all(&state.web_db)
        .await?;

    let mut points = Vec::new();

    for evaluation in evaluations {
        let eval_time_ms = (evaluation.updated_at - evaluation.created_at).num_milliseconds();

        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation.id))
            .all(&state.web_db)
            .await?;
        // Only sum actual build times; substituted builds contribute nothing.
        let build_time_total_ms: i64 = builds.iter().filter_map(|b| b.build_time_ms).sum();

        // Resolve entry-point builds for this evaluation.
        let ep_build_ids: Vec<BuildId> = EEntryPoint::find()
            .filter(CEntryPoint::Evaluation.eq(evaluation.id))
            .all(&state.web_db)
            .await?
            .into_iter()
            .map(|ep| ep.build)
            .collect();

        let ep_drv_ids: Vec<DerivationId> = if ep_build_ids.is_empty() {
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

        let entry_point_count = ep_drv_ids.len() as i64;
        let closure = derivation_closure_reachable(&state.web_db, ep_drv_ids.clone()).await?;
        let dependencies_count = (closure.len() as i64) - entry_point_count;

        let output_size_bytes = sum_output_sizes(&state.web_db, ep_drv_ids).await?;
        let closure_size_bytes =
            sum_output_sizes(&state.web_db, closure.into_iter().collect()).await?;

        points.push(ProjectMetricPoint {
            evaluation_id: evaluation.id,
            created_at: evaluation.created_at,
            build_time_total_ms,
            eval_time_ms,
            output_size_bytes,
            closure_size_bytes,
            dependencies_count,
        });
    }

    // Return in chronological order (oldest first for chart x-axis)
    points.reverse();

    Ok(ok_json(ProjectMetricsResponse {
        keep_evaluations: project.keep_evaluations,
        points,
    }))
}

// ── Per-entry-point metrics ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct EntryPointMetricsQuery {
    pub eval: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointMetricPoint {
    pub evaluation_id: EvaluationId,
    pub created_at: chrono::NaiveDateTime,
    pub build_status: entity::build::BuildStatus,
    pub build_time_ms: Option<i64>,
    pub output_size_bytes: Option<i64>,
    pub closure_size_bytes: Option<i64>,
    pub dependencies_count: i64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointMetricsResponse {
    pub eval: String,
    pub keep_evaluations: i32,
    pub points: Vec<EntryPointMetricPoint>,
}

/// Returns per-evaluation build metrics for a single entry point identified by its
/// `eval` attribute path (e.g. `packages.x86_64-linux.hello`).
pub async fn get_entry_point_metrics(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
    Query(params): Query<EntryPointMetricsQuery>,
) -> WebResult<Json<BaseResponse<EntryPointMetricsResponse>>> {
    let organization = load_org(
        &state.0,
        Caller::from_option(&maybe_user),
        organization,
        OrgAccess::Readable { label: "Project" },
    )
    .await?;

    let project = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(project))
        .one(&state.web_db)
        .await?
        .or_not_found("Project")?;

    let entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Project.eq(project.id))
        .filter(CEntryPoint::Eval.eq(&params.eval))
        .order_by_desc(CEntryPoint::CreatedAt)
        .limit(project.keep_evaluations as u64)
        .all(&state.web_db)
        .await?;

    let mut points = Vec::new();

    for ep in entry_points {
        let Some(evaluation) = EEvaluation::find_by_id(ep.evaluation)
            .one(&state.web_db)
            .await?
        else {
            continue;
        };

        let Some(build) = EBuild::find_by_id(ep.build).one(&state.web_db).await? else {
            continue;
        };

        // Substituted builds have build_time_ms = None; leave as null rather than
        // falling back to (updated_at - created_at) which gives ~0 ms.
        let build_time_ms = build.build_time_ms;

        let closure = derivation_closure_reachable(&state.web_db, vec![build.derivation]).await?;
        let dependencies_count = (closure.len() as i64).saturating_sub(1);

        let output_size_bytes = sum_output_sizes(&state.web_db, vec![build.derivation]).await?;
        let closure_size_bytes =
            sum_output_sizes(&state.web_db, closure.into_iter().collect()).await?;

        points.push(EntryPointMetricPoint {
            evaluation_id: evaluation.id,
            created_at: evaluation.created_at,
            build_status: build.status.for_api(),
            build_time_ms,
            output_size_bytes,
            closure_size_bytes,
            dependencies_count,
        });
    }

    points.reverse();

    Ok(ok_json(EntryPointMetricsResponse {
        eval: params.eval,
        keep_evaluations: project.keep_evaluations,
        points,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity::{cached_path, derivation_output};
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn now() -> chrono::NaiveDateTime {
        chrono::Utc::now().naive_utc()
    }

    fn drv_output(
        derivation: DerivationId,
        hash: &str,
        nar_size: Option<i64>,
        cached: Option<CachedPathId>,
    ) -> derivation_output::Model {
        derivation_output::Model {
            id: DerivationOutputId::now_v7(),
            derivation,
            name: "out".into(),
            output: format!("/nix/store/{hash}-foo"),
            hash: hash.into(),
            package: "foo".into(),
            ca: None,
            nar_size,
            is_cached: cached.is_some(),
            cached_path: cached,
            created_at: now(),
        }
    }

    fn cached_row(id: CachedPathId, hash: &str, nar_size: i64) -> cached_path::Model {
        cached_path::Model {
            id,
            store_path: format!("/nix/store/{hash}-foo"),
            hash: hash.into(),
            package: "foo".into(),
            file_hash: Some("sha256:dummy".into()),
            file_size: Some(nar_size / 2),
            nar_size: Some(nar_size),
            nar_hash: None,
            references: None,
            ca: None,
            deriver: None,
            created_at: now(),
        }
    }

    #[tokio::test]
    async fn sum_output_sizes_falls_back_to_cached_path_for_substituted() {
        let drv_id = DerivationId::now_v7();
        let cached_id = CachedPathId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![drv_output(drv_id, "abc", None, Some(cached_id))]])
            .append_query_results([vec![cached_row(cached_id, "abc", 1024)]])
            .into_connection();

        let total = sum_output_sizes(&db, vec![drv_id]).await.unwrap();
        assert_eq!(total, Some(1024));
    }

    #[tokio::test]
    async fn sum_output_sizes_mixes_built_and_substituted_outputs() {
        let drv_built = DerivationId::now_v7();
        let drv_sub = DerivationId::now_v7();
        let cached_id = CachedPathId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                drv_output(drv_built, "built", Some(2048), None),
                drv_output(drv_sub, "subst", None, Some(cached_id)),
            ]])
            .append_query_results([vec![cached_row(cached_id, "subst", 512)]])
            .into_connection();

        let total = sum_output_sizes(&db, vec![drv_built, drv_sub])
            .await
            .unwrap();
        assert_eq!(total, Some(2048 + 512));
    }

    #[tokio::test]
    async fn sum_output_sizes_returns_none_when_nothing_known() {
        let drv_id = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![drv_output(drv_id, "unknown", None, None)]])
            .append_query_results([Vec::<cached_path::Model>::new()])
            .into_connection();

        let total = sum_output_sizes(&db, vec![drv_id]).await.unwrap();
        assert_eq!(total, None);
    }
}
