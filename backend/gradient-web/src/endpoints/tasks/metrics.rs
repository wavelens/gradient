/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectAccess, load_project};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::builds::closure::sum_output_sizes;
use crate::error::WebResult;
use crate::helpers::{OptionExt, ok_json};
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::{
    begin_walk, output_hashes_for_drvs, runtime_closure_size, transitive_closure_reachable_in,
};
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct TaskMetricPoint {
    pub evaluation_id: EvaluationId,
    pub created_at: chrono::NaiveDateTime,
    pub build_time_total_ms: i64,
    pub eval_time_ms: i64,
    pub output_size_bytes: Option<i64>,
    pub closure_size_bytes: Option<i64>,
    pub runtime_closure_size_bytes: Option<i64>,
    pub dependencies_count: i64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TaskMetricsResponse {
    pub keep_evaluations: i32,
    pub points: Vec<TaskMetricPoint>,
}

// ── Endpoints ─────────────────────────────────────────────────────────────────

pub async fn get_task_metrics(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<TaskMetricsResponse>>> {
    let project = load_project(
        &state.0,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        project,
        ProjectAccess::Readable { label: "Task" },
    )
    .await?;

    let task = ETask::find()
        .filter(CTask::Project.eq(project.id))
        .filter(CTask::Name.eq(task))
        .one(&state.web_db)
        .await?
        .or_not_found("Task")?;

    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Task.eq(task.id))
        .filter(CEvaluation::Status.eq(gradient_entity::evaluation::EvaluationStatus::Completed))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(task.keep_evaluations as u64)
        .all(&state.web_db)
        .await?;

    // Anchors, their attempts and the entry points come back for the whole page
    // at once; only the closure walks below stay per evaluation, since each one
    // is seeded by that evaluation's own entry points.
    let eval_ids: Vec<EvaluationId> = evaluations.iter().map(|e| e.id).collect();

    let mut anchors_by_eval: HashMap<EvaluationId, Vec<DerivationBuildId>> = HashMap::new();
    for job in EBuildJob::find()
        .filter(CBuildJob::Evaluation.is_in(eval_ids.clone()))
        .all(&state.web_db)
        .await?
    {
        anchors_by_eval
            .entry(job.evaluation)
            .or_default()
            .push(job.derivation_build);
    }

    let all_anchors: Vec<DerivationBuildId> = anchors_by_eval.values().flatten().copied().collect();
    let attempts = gradient_db::latest_attempts(&state.web_db, &all_anchors).await?;

    let mut entry_points_by_eval: HashMap<EvaluationId, Vec<DerivationId>> = HashMap::new();
    for ep in EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.is_in(eval_ids))
        .all(&state.web_db)
        .await?
    {
        entry_points_by_eval
            .entry(ep.evaluation)
            .or_default()
            .push(ep.derivation);
    }

    let mut points = Vec::new();
    let walk = begin_walk(&state.web_db).await?;

    for evaluation in evaluations {
        let eval_time_ms = (evaluation.updated_at - evaluation.created_at).num_milliseconds();

        // Sum build time over every anchor this eval needs (one per build_job).
        let build_time_total_ms: i64 = anchors_by_eval
            .get(&evaluation.id)
            .into_iter()
            .flatten()
            .filter_map(|anchor| attempts.get(anchor))
            .filter_map(|a| a.duration_ms())
            .sum();

        let ep_drv_ids: Vec<DerivationId> = entry_points_by_eval
            .get(&evaluation.id)
            .cloned()
            .unwrap_or_default();

        let entry_point_count = ep_drv_ids.len() as i64;
        let closure = transitive_closure_reachable_in(&walk, &ep_drv_ids).await?;
        let dependencies_count = (closure.len() as i64) - entry_point_count;

        let output_size_bytes = sum_output_sizes(&state.web_db, ep_drv_ids.clone()).await?;
        let closure_size_bytes =
            sum_output_sizes(&state.web_db, closure.into_iter().collect()).await?;

        let seeds = output_hashes_for_drvs(&state.web_db, &ep_drv_ids).await?;
        let runtime = runtime_closure_size(&state.web_db, &seeds).await?;
        let runtime_closure_size_bytes = (runtime > 0).then_some(runtime);

        points.push(TaskMetricPoint {
            evaluation_id: evaluation.id,
            created_at: evaluation.created_at,
            build_time_total_ms,
            eval_time_ms,
            output_size_bytes,
            closure_size_bytes,
            runtime_closure_size_bytes,
            dependencies_count,
        });
    }

    walk.commit().await?;
    // Return in chronological order (oldest first for chart x-axis)
    points.reverse();

    Ok(ok_json(TaskMetricsResponse {
        keep_evaluations: task.keep_evaluations,
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
    /// Per-eval build identity (`build_job` id) for this entry point's derivation.
    pub build_id: BuildJobId,
    pub created_at: chrono::NaiveDateTime,
    pub build_status: gradient_entity::build::BuildStatus,
    pub build_time_ms: Option<i64>,
    pub output_size_bytes: Option<i64>,
    pub closure_size_bytes: Option<i64>,
    pub runtime_closure_size_bytes: Option<i64>,
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
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
    Query(params): Query<EntryPointMetricsQuery>,
) -> WebResult<Json<BaseResponse<EntryPointMetricsResponse>>> {
    let project = load_project(
        &state.0,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        project,
        ProjectAccess::Readable { label: "Task" },
    )
    .await?;

    let task = ETask::find()
        .filter(CTask::Project.eq(project.id))
        .filter(CTask::Name.eq(task))
        .one(&state.web_db)
        .await?
        .or_not_found("Task")?;

    let entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Task.eq(task.id))
        .filter(CEntryPoint::Eval.eq(&params.eval))
        .order_by_desc(CEntryPoint::CreatedAt)
        .limit(task.keep_evaluations as u64)
        .all(&state.web_db)
        .await?;

    // Evaluation, anchor, build job and attempt for the whole page in four
    // queries; the closure walks below stay per entry point, each seeded by its
    // own derivation. The build-job read is narrowed by derivation too, so it
    // returns the entry points rather than every build of every evaluation.
    let eval_ids: Vec<EvaluationId> = entry_points.iter().map(|ep| ep.evaluation).collect();
    let drv_ids: Vec<DerivationId> = entry_points.iter().map(|ep| ep.derivation).collect();

    let evaluations: HashMap<EvaluationId, MEvaluation> = EEvaluation::find()
        .filter(CEvaluation::Id.is_in(eval_ids.clone()))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|e| (e.id, e))
        .collect();

    let anchors: HashMap<DerivationId, MDerivationBuild> = EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.is_in(drv_ids.clone()))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|a| (a.derivation, a))
        .collect();

    let build_jobs: HashMap<(EvaluationId, DerivationId), MBuildJob> = EBuildJob::find()
        .filter(CBuildJob::Evaluation.is_in(eval_ids))
        .filter(CBuildJob::Derivation.is_in(drv_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|j| ((j.evaluation, j.derivation), j))
        .collect();

    let anchor_ids: Vec<DerivationBuildId> = anchors.values().map(|a| a.id).collect();
    let attempts = gradient_db::latest_attempts(&state.web_db, &anchor_ids)
        .await
        .unwrap_or_default();

    let mut points = Vec::new();
    let walk = begin_walk(&state.web_db).await?;

    for ep in entry_points {
        let (Some(evaluation), Some(anchor), Some(build_job)) = (
            evaluations.get(&ep.evaluation),
            anchors.get(&ep.derivation),
            build_jobs.get(&(ep.evaluation, ep.derivation)),
        ) else {
            continue;
        };

        let build_time_ms = attempts.get(&anchor.id).and_then(|a| a.duration_ms());

        let closure = transitive_closure_reachable_in(&walk, &[ep.derivation]).await?;
        let dependencies_count = (closure.len() as i64).saturating_sub(1);

        let output_size_bytes = sum_output_sizes(&state.web_db, vec![ep.derivation]).await?;
        let closure_size_bytes =
            sum_output_sizes(&state.web_db, closure.into_iter().collect()).await?;

        let seeds = output_hashes_for_drvs(&state.web_db, &[ep.derivation]).await?;
        let runtime = runtime_closure_size(&state.web_db, &seeds).await?;
        let runtime_closure_size_bytes = (runtime > 0).then_some(runtime);

        points.push(EntryPointMetricPoint {
            evaluation_id: evaluation.id,
            build_id: build_job.id,
            created_at: evaluation.created_at,
            build_status: anchor.status.for_api(),
            build_time_ms,
            output_size_bytes,
            closure_size_bytes,
            runtime_closure_size_bytes,
            dependencies_count,
        });
    }

    walk.commit().await?;
    points.reverse();

    Ok(ok_json(EntryPointMetricsResponse {
        eval: params.eval,
        keep_evaluations: task.keep_evaluations,
        points,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{cached_path, derivation_output};
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
            hash: hash.into(),
            package: "foo".into(),
            nar_size,
            is_cached: cached.is_some(),
            cached_path: cached,
            created_at: now(),
            ..Default::default()
        }
    }

    fn cached_row(id: CachedPathId, hash: &str, nar_size: i64) -> cached_path::Model {
        cached_path::Model {
            id,
            hash: hash.into(),
            package: "foo".into(),
            file_hash: Some("sha256:dummy".into()),
            file_size: Some(nar_size / 2),
            nar_size: Some(nar_size),
            created_at: now(),
            ..Default::default()
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
