/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::endpoints::get_org_readable;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectMetricPoint {
    pub evaluation_id: Uuid,
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
async fn derivation_closure_reachable(
    db: &sea_orm::DatabaseConnection,
    seed_drv_ids: Vec<Uuid>,
) -> WebResult<HashSet<Uuid>> {
    let mut visited: HashSet<Uuid> = seed_drv_ids.iter().cloned().collect();
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
/// `nar_size` is populated when the worker reports build output metadata;
/// `file_size` (compressed) is only populated per-cache, so it's unreliable
/// as a per-output total.
///
/// Returns `Some(total)` when the total is > 0, `None` otherwise.
async fn sum_output_sizes(
    db: &sea_orm::DatabaseConnection,
    drv_ids: Vec<Uuid>,
) -> WebResult<Option<i64>> {
    if drv_ids.is_empty() {
        return Ok(None);
    }
    let outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.is_in(drv_ids))
        .all(db)
        .await?;
    let total: i64 = outputs.iter().filter_map(|o| o.nar_size).sum();
    Ok(if total > 0 { Some(total) } else { None })
}

// ── Endpoints ─────────────────────────────────────────────────────────────────

pub async fn get_project_metrics(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<ProjectMetricsResponse>>> {
    let organization = get_org_readable(&state.0, organization, &maybe_user, "Project").await?;

    let project = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(project))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Project"))?;

    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(CEvaluation::Status.eq(entity::evaluation::EvaluationStatus::Completed))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(project.keep_evaluations as u64)
        .all(&state.db)
        .await?;

    let mut points = Vec::new();

    for evaluation in evaluations {
        let eval_time_ms = (evaluation.updated_at - evaluation.created_at).num_milliseconds();

        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation.id))
            .all(&state.db)
            .await?;
        // Only sum actual build times; substituted builds contribute nothing.
        let build_time_total_ms: i64 = builds.iter().filter_map(|b| b.build_time_ms).sum();

        // Resolve entry-point builds for this evaluation.
        let ep_build_ids: Vec<Uuid> = EEntryPoint::find()
            .filter(CEntryPoint::Evaluation.eq(evaluation.id))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|ep| ep.build)
            .collect();

        let ep_drv_ids: Vec<Uuid> = if ep_build_ids.is_empty() {
            vec![]
        } else {
            EBuild::find()
                .filter(CBuild::Id.is_in(ep_build_ids))
                .all(&state.db)
                .await?
                .into_iter()
                .map(|b| b.derivation)
                .collect()
        };

        let entry_point_count = ep_drv_ids.len() as i64;
        let closure = derivation_closure_reachable(&state.db, ep_drv_ids.clone()).await?;
        let dependencies_count = (closure.len() as i64) - entry_point_count;

        let output_size_bytes = sum_output_sizes(&state.db, ep_drv_ids).await?;
        let closure_size_bytes = sum_output_sizes(&state.db, closure.into_iter().collect()).await?;

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

    Ok(Json(BaseResponse {
        error: false,
        message: ProjectMetricsResponse {
            keep_evaluations: project.keep_evaluations,
            points,
        },
    }))
}

// ── Per-entry-point metrics ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct EntryPointMetricsQuery {
    pub eval: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointMetricPoint {
    pub evaluation_id: Uuid,
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
    let organization = get_org_readable(&state.0, organization, &maybe_user, "Project").await?;

    let project = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(project))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Project"))?;

    let entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Project.eq(project.id))
        .filter(CEntryPoint::Eval.eq(&params.eval))
        .order_by_desc(CEntryPoint::CreatedAt)
        .limit(project.keep_evaluations as u64)
        .all(&state.db)
        .await?;

    let mut points = Vec::new();

    for ep in entry_points {
        let Some(evaluation) = EEvaluation::find_by_id(ep.evaluation)
            .one(&state.db)
            .await?
        else {
            continue;
        };

        let Some(build) = EBuild::find_by_id(ep.build).one(&state.db).await? else {
            continue;
        };

        // Substituted builds have build_time_ms = None; leave as null rather than
        // falling back to (updated_at - created_at) which gives ~0 ms.
        let build_time_ms = build.build_time_ms;

        let closure = derivation_closure_reachable(&state.db, vec![build.derivation]).await?;
        let dependencies_count = (closure.len() as i64).saturating_sub(1);

        let output_size_bytes = sum_output_sizes(&state.db, vec![build.derivation]).await?;
        let closure_size_bytes = sum_output_sizes(&state.db, closure.into_iter().collect()).await?;

        points.push(EntryPointMetricPoint {
            evaluation_id: evaluation.id,
            created_at: evaluation.created_at,
            build_status: build.status,
            build_time_ms,
            output_size_bytes,
            closure_size_bytes,
            dependencies_count,
        });
    }

    points.reverse();

    Ok(Json(BaseResponse {
        error: false,
        message: EntryPointMetricsResponse {
            eval: params.eval,
            keep_evaluations: project.keep_evaluations,
            points,
        },
    }))
}
