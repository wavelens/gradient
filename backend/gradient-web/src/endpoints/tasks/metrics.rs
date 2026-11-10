/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::endpoints::builds::closure::sum_output_sizes;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;
use crate::scope::{Readable, TaskScope};
use axum::Json;
use axum::extract::{Query, State};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_db::graph::{
    closure::transitive_closure_reachable,
    runtime_closure::{output_hashes_for_drvs, runtime_closure_size},
};
use gradient_db::lookup::by_ids;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct TaskMetricPoint {
    pub evaluation_id: EvaluationId,
    pub created_at: chrono::NaiveDateTime,
    pub build_time_total_ms: i64,
    pub eval_time_ms: Option<i64>,
    pub output_size_bytes: Option<i64>,
    pub closure_size_bytes: Option<i64>,
    pub runtime_closure_size_bytes: Option<i64>,
    pub dependencies_count: i64,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct TaskMetricsResponse {
    pub keep_evaluations: i32,
    pub points: Vec<TaskMetricPoint>,
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/metrics",
    operation_id = "getTaskMetrics",
    tag = "tasks",
    summary = "Get task metrics",
    description = "Return metric points for each of the latest `keep_evaluations` evaluations that completed or failed. Measure `eval_time_ms` from fetch start to finish and exclude queue time. Retries skip fetching and start their `eval_time_ms` at build start. Evaluations that never started use `null` for `eval_time_ms`.",
    params(("project" = String, Path), ("task" = String, Path)),
    responses((status = OK, description = "Task metrics", body = TaskMetricsResponse), WebError)
)]
pub async fn get_task_metrics(
    TaskScope { task, .. }: TaskScope<Readable>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<TaskMetricsResponse>>> {
    let evaluations = gradient_db::evaluations::get_newest_completed_or_failed_task_evaluations(
        &state.web_db,
        task.id,
        task.keep_evaluations as u64,
    )
    .await?;

    let eval_ids: Vec<EvaluationId> = evaluations.iter().map(|e| e.id).collect();

    let mut shared_builds_by_eval: HashMap<EvaluationId, Vec<DerivationBuildId>> = HashMap::new();
    for job in gradient_db::build_jobs::of_evaluations(&state.web_db, &eval_ids).await? {
        shared_builds_by_eval
            .entry(job.evaluation)
            .or_default()
            .push(job.derivation_build);
    }

    let all_shared_builds: Vec<DerivationBuildId> =
        shared_builds_by_eval.values().flatten().copied().collect();
    let attempts =
        gradient_db::scheduling::build_attempt::latest_attempts(&state.web_db, &all_shared_builds)
            .await?;

    let mut entry_points_by_eval: HashMap<EvaluationId, Vec<DerivationId>> = HashMap::new();
    for ep in gradient_db::entry_points::of_evaluations(&state.web_db, &eval_ids).await? {
        entry_points_by_eval
            .entry(ep.evaluation)
            .or_default()
            .push(ep.derivation);
    }

    // Each iteration is opening and committing its own sized walk. One walk across the loop would
    // pin a web pool connection for minutes at nixpkgs scale.
    let mut points = Vec::new();
    for evaluation in evaluations {
        let eval_time_ms = evaluation
            .started_at()
            .zip(evaluation.finished_at)
            .map(|(started, finished)| round_time_delta_to_millis(finished - started));

        let build_time_total_ms: i64 = shared_builds_by_eval
            .get(&evaluation.id)
            .into_iter()
            .flatten()
            .filter_map(|shared_build| attempts.get(shared_build))
            .filter_map(|a| a.duration_ms())
            .sum();

        let ep_drv_ids: Vec<DerivationId> = entry_points_by_eval
            .get(&evaluation.id)
            .cloned()
            .unwrap_or_default();

        let entry_point_count = ep_drv_ids.len() as i64;
        let closure = transitive_closure_reachable(&state.web_db, &ep_drv_ids).await?;
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

    points.reverse();

    Ok(ok_json(TaskMetricsResponse {
        keep_evaluations: task.keep_evaluations,
        points,
    }))
}

/// These walks are reading global tables with no evaluation column. This is making `DerivationId` a
/// sound memo key. Scoping the walk to the evaluation would require an `(EvaluationId,
/// DerivationId)` key.
#[derive(Clone, Copy)]
struct DerivationMetrics {
    dependencies_count: i64,
    output_size_bytes: Option<i64>,
    closure_size_bytes: Option<i64>,
    runtime_closure_size_bytes: Option<i64>,
}

async fn derivation_metrics(db: &WebDb, derivation: DerivationId) -> WebResult<DerivationMetrics> {
    let closure = transitive_closure_reachable(db, &[derivation]).await?;
    let dependencies_count = (closure.len() as i64).saturating_sub(1);
    let output_size_bytes = sum_output_sizes(db, vec![derivation]).await?;
    let closure_size_bytes = sum_output_sizes(db, closure.into_iter().collect()).await?;
    let seeds = output_hashes_for_drvs(db, &[derivation]).await?;
    let runtime = runtime_closure_size(db, &seeds).await?;

    Ok(DerivationMetrics {
        dependencies_count,
        output_size_bytes,
        closure_size_bytes,
        runtime_closure_size_bytes: (runtime > 0).then_some(runtime),
    })
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EntryPointMetricsQuery {
    pub eval: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct EntryPointMetricPoint {
    pub evaluation_id: EvaluationId,
    pub build_id: BuildJobId,
    pub created_at: chrono::NaiveDateTime,
    pub build_status: gradient_entity::build::BuildStatus,
    pub build_time_ms: Option<i64>,
    pub output_size_bytes: Option<i64>,
    pub closure_size_bytes: Option<i64>,
    pub runtime_closure_size_bytes: Option<i64>,
    pub dependencies_count: i64,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct EntryPointMetricsResponse {
    pub eval: String,
    pub keep_evaluations: i32,
    pub points: Vec<EntryPointMetricPoint>,
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/entry-point-metrics",
    operation_id = "getEntryPointMetrics",
    tag = "tasks",
    summary = "Get entry point metrics",
    description = "Returns per-evaluation build metrics for a single entry point identified by its\nNix attribute path (`eval`), for the most recent `keep_evaluations` completed or failed evaluations.\n\nThe `eval` value is the full Nix attribute path as stored in the database, which may\ncontain double-quoted segments for attribute names with special characters\n(e.g. `packages.\"x86_64-linux\".hello`). URL-encode `\"` as `%22` in query strings.\n\nReturn 404 if none of the evaluations of the task contain an entry point with this `eval`.",
    params(("project" = String, Path), ("task" = String, Path), EntryPointMetricsQuery),
    responses((status = OK, description = "Entry point metrics", body = EntryPointMetricsResponse), WebError)
)]
pub async fn get_entry_point_metrics(
    TaskScope { task, .. }: TaskScope<Readable>,
    state: State<Arc<ServerState>>,
    Query(params): Query<EntryPointMetricsQuery>,
) -> WebResult<Json<BaseResponse<EntryPointMetricsResponse>>> {
    let entry_points = gradient_db::entry_points::newest_entry_points_for_task_attribute_path(
        &state.web_db,
        task.id,
        &params.eval,
        task.keep_evaluations as u64,
    )
    .await?;
    if entry_points.is_empty()
        && !gradient_db::entry_points::any_evaluation_ever_has_entry_point_with_attribute(
            &state.web_db,
            task.id,
            &params.eval,
        )
        .await?
    {
        return Err(WebError::not_found(Resource::EntryPoint));
    }

    let eval_ids: Vec<EvaluationId> = entry_points.iter().map(|ep| ep.evaluation).collect();
    let drv_ids: Vec<DerivationId> = entry_points.iter().map(|ep| ep.derivation).collect();

    let evaluations: HashMap<EvaluationId, MEvaluation> =
        by_ids::<EEvaluation, _>(&state.web_db, &eval_ids).await?;

    let shared_builds: HashMap<DerivationId, MDerivationBuild> =
        gradient_db::derivation_builds::of_derivations(&state.web_db, &drv_ids)
            .await?
            .into_iter()
            .map(|a| (a.derivation, a))
            .collect();

    let build_jobs: HashMap<(EvaluationId, DerivationId), MBuildJob> =
        gradient_db::build_jobs::of_derivations_in_evaluations(&state.web_db, &eval_ids, &drv_ids)
            .await?
            .into_iter()
            .map(|j| ((j.evaluation, j.derivation), j))
            .collect();

    let shared_build_ids: Vec<DerivationBuildId> = shared_builds.values().map(|a| a.id).collect();
    let attempts =
        gradient_db::scheduling::build_attempt::latest_attempts(&state.web_db, &shared_build_ids)
            .await?;

    let mut by_derivation: HashMap<DerivationId, DerivationMetrics> = HashMap::new();
    let mut points = Vec::new();
    for ep in entry_points {
        let (Some(evaluation), Some(shared_build), Some(build_job)) = (
            evaluations.get(&ep.evaluation),
            shared_builds.get(&ep.derivation),
            build_jobs.get(&(ep.evaluation, ep.derivation)),
        ) else {
            continue;
        };

        let build_time_ms = attempts.get(&shared_build.id).and_then(|a| a.duration_ms());

        let metrics = match by_derivation.get(&ep.derivation) {
            Some(m) => *m,
            None => {
                let m = derivation_metrics(&state.0.web_db, ep.derivation).await?;
                by_derivation.insert(ep.derivation, m);
                m
            }
        };

        points.push(EntryPointMetricPoint {
            evaluation_id: evaluation.id,
            build_id: build_job.id,
            created_at: evaluation.created_at,
            build_status: shared_build.status.for_api(),
            build_time_ms,
            output_size_bytes: metrics.output_size_bytes,
            closure_size_bytes: metrics.closure_size_bytes,
            runtime_closure_size_bytes: metrics.runtime_closure_size_bytes,
            dependencies_count: metrics.dependencies_count,
        });
    }

    points.reverse();

    Ok(ok_json(EntryPointMetricsResponse {
        eval: params.eval,
        keep_evaluations: task.keep_evaluations,
        points,
    }))
}

fn round_time_delta_to_millis(delta: chrono::TimeDelta) -> i64 {
    (delta + chrono::TimeDelta::microseconds(500)).num_milliseconds()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{cached_path, derivation_output};
    use gradient_fixtures::mock::mock_db;

    #[test]
    fn eval_time_rounds_to_the_nearest_millisecond() {
        let micros = chrono::TimeDelta::microseconds;
        assert_eq!(round_time_delta_to_millis(micros(611_600)), 612);
        assert_eq!(round_time_delta_to_millis(micros(611_400)), 611);
    }

    fn now() -> chrono::NaiveDateTime {
        chrono::Utc::now().naive_utc()
    }

    fn drv_output(
        derivation: DerivationId,
        hash: &str,
        nar_size: Option<i64>,
    ) -> derivation_output::Model {
        derivation_output::Model {
            id: DerivationOutputId::now_v7(),
            derivation,
            name: "out".into(),
            hash: Some(hash.into()),
            package: Some("foo".into()),
            nar_size,
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
        let db = mock_db()
            .append_query_results([vec![drv_output(drv_id, "abc", None)]])
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
        let db = mock_db()
            .append_query_results([vec![
                drv_output(drv_built, "built", Some(2048)),
                drv_output(drv_sub, "subst", None),
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
        let db = mock_db()
            .append_query_results([vec![drv_output(drv_id, "unknown", None)]])
            .append_query_results([Vec::<cached_path::Model>::new()])
            .into_connection();

        let total = sum_output_sizes(&db, vec![drv_id]).await.unwrap();
        assert_eq!(total, None);
    }
}

#[utoipa::path(
    get,
    path = "/metrics/tasks/{project}/{task}/evaluations",
    operation_id = "getTaskEvaluationMetrics",
    tag = "board",
    summary = "Per-evaluation task metrics",
    description = "A per-evaluation series for recent completed or failed evaluations of a task (build time, evaluation time, output/closure size, dependencies).",
    params(("project" = String, Path), ("task" = String, Path)),
    responses((status = OK, description = "Per-evaluation metric points", body = TaskMetricsResponse), WebError)
)]
pub async fn task_evaluation_metrics(
    scope: TaskScope<Readable>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<TaskMetricsResponse>>> {
    get_task_metrics(scope, state).await
}

#[utoipa::path(
    get,
    path = "/metrics/tasks/{project}/{task}/entry-point",
    operation_id = "getEntryPointEvaluationMetrics",
    tag = "board",
    summary = "Per-evaluation entry-point metrics",
    description = "Per-evaluation build metrics for a single entry point identified by its `eval` attribute path.",
    params(("project" = String, Path), ("task" = String, Path), EntryPointMetricsQuery),
    responses((status = OK, description = "Per-evaluation entry-point metric points", body = EntryPointMetricsResponse), WebError)
)]
pub async fn task_entry_point_metrics(
    scope: TaskScope<Readable>,
    state: State<Arc<ServerState>>,
    query: Query<EntryPointMetricsQuery>,
) -> WebResult<Json<BaseResponse<EntryPointMetricsResponse>>> {
    get_entry_point_metrics(scope, state, query).await
}
