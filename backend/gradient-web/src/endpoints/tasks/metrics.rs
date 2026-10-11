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
use crate::resource::Resource;
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
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

pub async fn get_task_metrics(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<TaskMetricsResponse>>> {
    let project = load_project(
        &state.0.db(),
        Caller::from(&maybe_user),
        api_key.as_ref(),
        project,
        ProjectAccess::Readable {
            label: Resource::Task,
        },
    )
    .await?;

    let task = gradient_db::tasks::by_name(&state.web_db, project.id, &task)
        .await?
        .or_not_found(Resource::Task)?;

    let evaluations = gradient_db::evaluations::latest_completed_of_task(
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
        let eval_time_ms = (evaluation.updated_at - evaluation.created_at).num_milliseconds();

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

#[derive(Deserialize)]
pub struct EntryPointMetricsQuery {
    pub eval: String,
}

#[derive(Serialize, Deserialize, Debug)]
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

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointMetricsResponse {
    pub eval: String,
    pub keep_evaluations: i32,
    pub points: Vec<EntryPointMetricPoint>,
}

pub async fn get_entry_point_metrics(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
    Query(params): Query<EntryPointMetricsQuery>,
) -> WebResult<Json<BaseResponse<EntryPointMetricsResponse>>> {
    let project = load_project(
        &state.0.db(),
        Caller::from(&maybe_user),
        api_key.as_ref(),
        project,
        ProjectAccess::Readable {
            label: Resource::Task,
        },
    )
    .await?;

    let task = gradient_db::tasks::by_name(&state.web_db, project.id, &task)
        .await?
        .or_not_found(Resource::Task)?;

    let entry_points = gradient_db::entry_points::latest_of_attribute(
        &state.web_db,
        task.id,
        &params.eval,
        task.keep_evaluations as u64,
    )
    .await?;

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
            .await
            .unwrap_or_default();

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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{cached_path, derivation_output};
    use gradient_fixtures::mock::mock_db;

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
        let db = mock_db()
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
        let db = mock_db()
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
        let db = mock_db()
            .append_query_results([vec![drv_output(drv_id, "unknown", None, None)]])
            .append_query_results([Vec::<cached_path::Model>::new()])
            .into_connection();

        let total = sum_output_sizes(&db, vec![drv_id]).await.unwrap();
        assert_eq!(total, None);
    }
}
