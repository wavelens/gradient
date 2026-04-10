/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use core::db::get_any_organization_by_name;
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

pub async fn get_project_metrics(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<ProjectMetricsResponse>>> {
    let organization = get_any_organization_by_name(state.0.clone(), organization)
        .await?
        .ok_or_else(|| WebError::not_found("Organization"))?;

    if !organization.public {
        match &maybe_user {
            Some(user) => {
                if !user_is_org_member(&state.0, user.id, organization.id).await? {
                    return Err(WebError::not_found("Project"));
                }
            }
            None => return Err(WebError::not_found("Project")),
        }
    }

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

        // Sum build durations for all completed builds
        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation.id))
            .all(&state.db)
            .await?;

        // Only sum actual build times; substituted builds have build_time_ms = None
        // and should not contribute (they had no build cost).
        let build_time_total_ms: i64 = builds.iter().filter_map(|b| b.build_time_ms).sum();

        // Entry points for this evaluation
        let ep_build_ids: Vec<Uuid> = EEntryPoint::find()
            .filter(CEntryPoint::Evaluation.eq(evaluation.id))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|ep| ep.build)
            .collect();

        // Resolve entry-point builds to their derivations.
        let ep_drv_ids: Vec<Uuid> = if ep_build_ids.is_empty() {
            vec![]
        } else {
            EBuild::find()
                .filter(CBuild::Id.is_in(ep_build_ids.clone()))
                .all(&state.db)
                .await?
                .into_iter()
                .map(|b| b.derivation)
                .collect()
        };

        let entry_point_count = ep_drv_ids.len() as i64;

        // BFS over derivation_dependency — graph is authoritative across evals.
        let mut all_reachable: HashSet<Uuid> = ep_drv_ids.iter().cloned().collect();
        let mut frontier: Vec<Uuid> = ep_drv_ids.clone();
        while !frontier.is_empty() {
            let edges = EDerivationDependency::find()
                .filter(CDerivationDependency::Derivation.is_in(frontier.clone()))
                .all(&state.db)
                .await?;
            frontier.clear();
            for edge in edges {
                if all_reachable.insert(edge.dependency) {
                    frontier.push(edge.dependency);
                }
            }
        }
        let dependencies_count = (all_reachable.len() as i64) - entry_point_count;

        // Output size: file_size of entry-point derivation outputs only
        let output_size_bytes = if ep_drv_ids.is_empty() {
            None
        } else {
            let outputs = EDerivationOutput::find()
                .filter(CDerivationOutput::Derivation.is_in(ep_drv_ids))
                .all(&state.db)
                .await?;
            let total: i64 = outputs.iter().filter_map(|o| o.file_size).sum();
            if total > 0 { Some(total) } else { None }
        };

        // Closure size: file_size of outputs of all reachable derivations
        let closure_size_bytes = if all_reachable.is_empty() {
            None
        } else {
            let outputs = EDerivationOutput::find()
                .filter(
                    CDerivationOutput::Derivation
                        .is_in(all_reachable.into_iter().collect::<Vec<_>>()),
                )
                .all(&state.db)
                .await?;
            let total: i64 = outputs.iter().filter_map(|o| o.file_size).sum();
            if total > 0 { Some(total) } else { None }
        };

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
    let organization = get_any_organization_by_name(state.0.clone(), organization)
        .await?
        .ok_or_else(|| WebError::not_found("Organization"))?;

    if !organization.public {
        match &maybe_user {
            Some(user) => {
                if !user_is_org_member(&state.0, user.id, organization.id).await? {
                    return Err(WebError::not_found("Project"));
                }
            }
            None => return Err(WebError::not_found("Project")),
        }
    }

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

        // Output size: only this derivation's outputs
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(build.derivation))
            .all(&state.db)
            .await?;
        let output_total: i64 = outputs.iter().filter_map(|o| o.file_size).sum();
        let output_size_bytes = if output_total > 0 {
            Some(output_total)
        } else {
            None
        };

        // Substituted builds have build_time_ms = None; leave as null rather than
        // falling back to (updated_at - created_at) which gives ~0 ms.
        let build_time_ms = build.build_time_ms;

        // BFS over derivation_dependency.
        let mut visited: HashSet<Uuid> = HashSet::new();
        let mut frontier = vec![build.derivation];
        visited.insert(build.derivation);
        while !frontier.is_empty() {
            let edges = EDerivationDependency::find()
                .filter(CDerivationDependency::Derivation.is_in(frontier.clone()))
                .all(&state.db)
                .await?;
            frontier.clear();
            for edge in edges {
                if visited.insert(edge.dependency) {
                    frontier.push(edge.dependency);
                }
            }
        }

        let dependencies_count = (visited.len() as i64).saturating_sub(1);

        let closure_outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(visited.into_iter().collect::<Vec<_>>()))
            .all(&state.db)
            .await?;
        let closure_total: i64 = closure_outputs.iter().filter_map(|o| o.file_size).sum();
        let closure_size_bytes = if closure_total > 0 {
            Some(closure_total)
        } else {
            None
        };

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
