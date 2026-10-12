/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TaskRefusal;
use super::{
    BuildStatusCounts, EntryPointSummary, EvaluationSummary, EvaluationTriggerSummary,
    FailedAttributeSummary, PaginatedEntryPoints, QueueSummary, TaskDetailsResponse,
};
use crate::access::{has_permission, is_project_member};
use crate::api_spec::{Binary, TaskPath};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::evals::{live_progress, live_progress_status};
use crate::endpoints::{archive_headers, build_product_headers};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::lookup::require;
use crate::permissions::Permission;
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{Readable, Requires, TaskScope, can};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_db::evaluations::TaskEvaluationFilter;
use gradient_db::lookup::{by_id, by_ids, get_any_project_by_name};
use gradient_entity::build::BuildStatus;
use gradient_entity::derivation_output::UNKNOWN_OUTPUT_HASH;
use gradient_entity::evaluation::{EvaluationStatus, WalkMode};
use gradient_entity::evaluation_message::MessageLevel;
use gradient_sources::{get_commit_info, head_commit};
use gradient_storage::NarStore;
use gradient_storage::nar_extract::{
    ExtractError, Extracted, extract_path_from_reader, nar_reader_from_stream,
};
use gradient_types::input::{hex_to_vec, vec_to_hex};
use gradient_types::*;
use gradient_util::latest::Latest;
use sea_orm::Iterable;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

#[derive(Deserialize, Default, utoipa::ToSchema)]
pub struct EvaluateRequest {
    pub commit: Option<String>,
    pub attr: Option<String>,
    pub walk: Option<WalkMode>,
}

pub(super) async fn evaluations_to_summaries(
    web_db: &WebDb,
    eval_progress: &Latest<EvaluationId, EvaluationProgress>,
    evaluations: Vec<MEvaluation>,
) -> Result<Vec<EvaluationSummary>, WebError> {
    if evaluations.is_empty() {
        return Ok(Vec::new());
    }

    let db = web_db;
    let eval_ids: Vec<EvaluationId> = evaluations.iter().map(|e| e.id).collect();

    let trigger_ids: Vec<TaskTriggerId> = evaluations.iter().filter_map(|e| e.trigger).collect();
    let triggers: HashMap<TaskTriggerId, TriggerType> = by_ids::<ETaskTrigger, _>(db, &trigger_ids)
        .await?
        .into_iter()
        .map(|(id, t)| (id, t.trigger_type))
        .collect();

    let commit_ids: Vec<CommitId> = evaluations.iter().map(|e| e.commit).collect();
    let commits: HashMap<CommitId, MCommit> = by_ids::<ECommit, _>(db, &commit_ids).await?;

    let user_ids: Vec<UserId> = evaluations.iter().filter_map(|e| e.started_by).collect();
    let user_names: HashMap<UserId, String> = by_ids::<EUser, _>(db, &user_ids)
        .await?
        .into_iter()
        .map(|(id, u)| (id, u.name))
        .collect();

    let status_counts =
        gradient_db::task_board::build_status_counts_by_evaluation(db, &eval_ids).await?;
    let message_counts = gradient_db::task_board::evaluation_message_counts(db, &eval_ids).await?;
    let eval_jobs =
        gradient_db::scheduling::assignment_record::latest_eval_jobs(db, &eval_ids).await?;
    let with_qos = gradient_db::scheduling::priority::evaluations_with_qos(db, &eval_ids).await?;
    let expected_thunks = match evaluations
        .iter()
        .find(|e| live_progress_status(e.status))
        .and_then(|e| e.task)
    {
        Some(task) => gradient_db::evaluations::expected_thunks::expected_thunks(db, task).await?,
        None => None,
    };

    let mut out = Vec::with_capacity(evaluations.len());
    for evaluation in evaluations {
        let commit = commits.get(&evaluation.commit);
        let commit_hash = commit.map(|c| vec_to_hex(&c.hash)).unwrap_or_default();
        let commit_message = commit.and_then(|c| first_line_truncated(&c.message, 100));

        let mut builds = BuildStatusCounts::default();
        if let Some(per_status) = status_counts.get(&evaluation.id) {
            for (status, n) in per_status {
                builds.add(*status, *n);
            }
        }

        let msgs = message_counts.get(&evaluation.id);
        let errors = msgs
            .and_then(|m| m.get(&MessageLevel::Error))
            .copied()
            .unwrap_or(0);
        let warnings = msgs
            .and_then(|m| m.get(&MessageLevel::Warning))
            .copied()
            .unwrap_or(0);

        let trigger = evaluation.trigger.and_then(|tid| {
            triggers
                .get(&tid)
                .map(|&tt| EvaluationTriggerSummary::new(tid, tt))
        });
        let triggered_by = evaluation
            .started_by
            .and_then(|uid| user_names.get(&uid).cloned());

        let pr_number = evaluation
            .source_comment
            .as_ref()
            .and_then(|v| v.get("pr_number"))
            .or_else(|| {
                evaluation
                    .waiting_reason
                    .as_ref()
                    .and_then(|v| v.get("pr_number"))
            })
            .and_then(|n| n.as_u64());

        out.push(EvaluationSummary {
            id: evaluation.id,
            commit: commit_hash,
            commit_message,
            status: evaluation.status,
            wildcard: evaluation.wildcard.clone(),
            trigger,
            triggered_by,
            pr_number,
            total_builds: builds.total(),
            builds,
            errors,
            warnings,
            dispatched_job: eval_jobs.get(&evaluation.id).copied(),
            prioritized: with_qos.contains(&evaluation.id),
            created_at: evaluation.created_at,
            started_at: evaluation.fetch_started_at,
            finished_at: evaluation.finished_at,
            updated_at: evaluation.updated_at,
            progress: live_progress(
                eval_progress,
                evaluation.id,
                evaluation.status,
                Instant::now(),
            ),
            expected_thunks: expected_thunks.filter(|_| live_progress_status(evaluation.status)),
        });
    }
    Ok(out)
}

fn first_line_truncated(s: &str, max: usize) -> Option<String> {
    let line = s.lines().find(|l| !l.trim().is_empty())?.trim();
    Some(line.chars().take(max).collect())
}

#[utoipa::path(
    post,
    path = "/tasks/{project}/{task}/evaluate",
    operation_id = "triggerEvaluation",
    tag = "tasks",
    summary = "Trigger evaluation",
    description = "Queues a new evaluation for the task and returns its UUID in `message`,\nso a caller can follow the run it just started (`GET /evals/{id}`, or the\n`GET /evals/{id}/live` stream). `POST /evals/{evaluation}/retry` re-runs\nthe failed builds of a finished evaluation.\n\nReturns `400` with `code: \"repository_unreachable\"` when the repository\ncannot be fetched (DNS failure, authentication failure, unreachable host,\nbad URL, …) - the message includes the underlying git error.",
    params(TaskPath),
    request_body = Option<EvaluateRequest>,
    responses((status = OK, description = "Evaluation UUID", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_task_evaluate(
    TaskScope { task, user, db, .. }: TaskScope<Requires<can::TriggerEvaluation>>,
    state: State<Arc<ServerState>>,
    body: Option<Json<EvaluateRequest>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let walk_mode = body.as_ref().and_then(|b| b.walk).unwrap_or_default();

    let pinned = body
        .as_ref()
        .and_then(|b| b.commit.as_deref())
        .map(|commit| {
            hex_to_vec(commit)
                .ok()
                .filter(|h| h.len() == COMMIT_HASH_BYTES)
                .ok_or(TaskRefusal::InvalidCommitHash)
        })
        .transpose()?;

    let attr = body
        .as_ref()
        .and_then(|b| b.attr.as_deref())
        .map(|a| {
            a.parse::<Wildcard>()
                .map(|w| w.to_string())
                .map_err(|e| TaskRefusal::InvalidAttr {
                    reason: e.to_string(),
                })
        })
        .transpose()?;

    let pinned_run = pinned.is_some();

    let (commit_hash, commit_message, author_name) = match pinned {
        Some(commit_hash) => {
            let commit = get_commit_info(&db, &task, &commit_hash)
                .await
                .map_err(|e| TaskRefusal::CommitLookupFailed {
                    reason: e.to_string(),
                })?;
            (commit_hash, commit.message, commit.author_name)
        }
        None => {
            let head = head_commit(&db, &task, None).await.map_err(|e| {
                SharedRefusal::RepositoryStateFetchFailed {
                    reason: e.to_string(),
                }
            })?;
            let commit_hash = head.hash;

            if let Err(e) = gradient_ci::trigger::maybe_trigger_input_update(
                &state.web_db,
                &task,
                commit_hash.clone(),
                None,
            )
            .await
            {
                tracing::warn!(error = %e, task = %task.name, "manual input_update trigger failed");
            }

            (commit_hash, head.message, head.author_name)
        }
    };

    // A pinned run is an out-of-band request, typically from a deployment tool. Marking it
    // concurrent is keeping it from queueing behind or aborting the task's own CI run.
    let concurrent = pinned_run;

    let eval = gradient_ci::trigger_evaluation(
        &state.web_db,
        &task,
        commit_hash,
        Some(commit_message),
        Some(author_name),
        None,
        concurrent,
        None,
        attr,
        None,
        Some(user.id),
        walk_mode,
    )
    .await
    .map_err(|e| match e {
        gradient_ci::TriggerError::AlreadyInProgress => {
            WebError::from(TaskRefusal::EvaluationInProgress)
        }
        gradient_ci::TriggerError::Db(db_err) => WebError::from(db_err),
    })?;

    let eval = gradient_ci::park_if_no_cache(&state.web_db, eval, task.project).await?;
    let eval = gradient_ci::park_if_storage_full(
        &state.web_db,
        eval,
        task.project,
        state.config.cache.max_storage_gb,
    )
    .await?;
    let eval = gradient_ci::park_if_no_workers(&state.web_db, eval, task.project).await?;
    state.record_evaluation_created(&eval).await;

    Ok(ok_json(eval.id.to_string()))
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/evaluations",
    operation_id = "getTaskEvaluations",
    tag = "tasks",
    summary = "List and search evaluations",
    description = "Returns the `keep_evaluations` most recent evaluations for the task,\nnewest first. The `commit`, `status` and `attr` filters combine (AND) to\nanswer \"has this attribute at this commit been evaluated, and is it still\nrunning?\" without walking the whole history.\n\n`attr` matches an evaluation's **wildcard**, not its entry points. The\nwildcard is set when the evaluation row is created, so the filter answers\nwhile the evaluation is still `Queued` or `EvaluatingFlake`; entry points\ndo not exist until derivations have resolved. The flip side is that a\nmatch means the attribute was *in scope*, not that it exists in the flake\nor that it built - read `/tasks/{project}/{task}/entry-points` for that.",
    params(TaskPath, EvaluationsQuery),
    responses((status = OK, description = "List of evaluation summaries", body = Vec<EvaluationSummary>), WebError)
)]
pub async fn get_task_evaluations(
    TaskScope { task, .. }: TaskScope<Readable>,
    state: State<Arc<ServerState>>,
    Query(params): Query<EvaluationsQuery>,
) -> WebResult<Json<BaseResponse<Vec<EvaluationSummary>>>> {
    let limit = params.limit.unwrap_or(task.keep_evaluations as u64);
    let attr = params.attr.as_deref().map(parse_attr_filter).transpose()?;

    let mut filter = TaskEvaluationFilter::default();

    if let Some(before) = params.before {
        let cursor = gradient_db::evaluations::by_id_in_task(&state.web_db, task.id, before)
            .await?
            .or_not_found(Resource::Evaluation)?;
        filter.older_than = Some(cursor);
    }

    if let Some(commit) = params.commit.as_deref() {
        let hash = hex_to_vec(commit)
            .ok()
            .filter(|h| h.len() == COMMIT_HASH_BYTES)
            .ok_or(TaskRefusal::InvalidCommitHash)?;

        let commit_ids: Vec<CommitId> = gradient_db::commits::with_hash(&state.web_db, hash)
            .await?
            .into_iter()
            .map(|c| c.id)
            .collect();

        if commit_ids.is_empty() {
            return Ok(ok_json(vec![]));
        }

        filter.commits = Some(commit_ids);
    }

    if let Some(status) = params.status.as_deref() {
        filter.statuses = Some(parse_status_filter(status)?);
    }

    let evaluations = match attr {
        Some(attr) => {
            gradient_db::evaluations::page_of_task(&state.web_db, task.id, filter, ATTR_SCAN_LIMIT)
                .await?
                .into_iter()
                .filter(|e| {
                    e.wildcard
                        .parse::<Wildcard>()
                        .is_ok_and(|w| w.matches(attr))
                })
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .collect()
        }
        None => {
            gradient_db::evaluations::page_of_task(&state.web_db, task.id, filter, limit).await?
        }
    };

    let summaries =
        evaluations_to_summaries(&state.0.web_db, &state.0.memory.eval_progress, evaluations)
            .await?;

    Ok(ok_json(summaries))
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/details",
    operation_id = "getTaskDetails",
    tag = "tasks",
    summary = "Get task details",
    description = "Returns aggregated task data including the latest evaluation, recent evaluation history, and current entry points.",
    params(TaskPath),
    responses((status = OK, description = "Task details", body = TaskDetailsResponse), WebError)
)]
pub async fn get_task_details(
    TaskScope {
        project,
        task,
        user: maybe_user,
        api_key,
        ..
    }: TaskScope<Readable>,
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<TaskDetailsResponse>>> {
    let api_key_ref = api_key.as_ref();

    let evaluations = gradient_db::evaluations::latest_of_task(&state.web_db, task.id, 10).await?;

    let evaluation_summaries =
        evaluations_to_summaries(&state.0.web_db, &state.0.memory.eval_progress, evaluations)
            .await?;

    let (building, queued) =
        gradient_db::task_board::task_queue_summary(&state.web_db, task.id).await?;

    let (can_edit, can_trigger) = match &maybe_user {
        Some(user) => (
            has_permission(
                &state.web_db,
                user.id,
                project.id,
                Permission::EditTask,
                api_key_ref,
            )
            .await?,
            has_permission(
                &state.web_db,
                user.id,
                project.id,
                Permission::TriggerEvaluation,
                api_key_ref,
            )
            .await?,
        ),
        None => (false, false),
    };

    let task_details = TaskDetailsResponse::new(
        task,
        QueueSummary { building, queued },
        evaluation_summaries,
        can_edit,
        can_trigger,
    );

    let res = BaseResponse {
        error: false,
        message: task_details,
    };

    Ok(Json(res))
}

#[derive(Deserialize, Debug, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EvaluationsQuery {
    pub limit: Option<u64>,
    pub before: Option<EvaluationId>,
    pub commit: Option<String>,
    pub status: Option<String>,
    pub attr: Option<String>,
}

const COMMIT_HASH_BYTES: usize = 20;

const ATTR_SCAN_LIMIT: u64 = 1000;

fn parse_attr_filter(attr: &str) -> WebResult<&str> {
    let unquoted_has = |c: char| attr.split('"').step_by(2).any(|part| part.contains(c));

    if attr.is_empty() || attr.contains(',') || unquoted_has('*') || unquoted_has('#') {
        return Err(TaskRefusal::AttrNotConcrete.into());
    }

    Ok(attr)
}

fn parse_status_filter(raw: &str) -> WebResult<Vec<EvaluationStatus>> {
    match raw {
        "active" => Ok(EvaluationStatus::ACTIVE.to_vec()),
        "terminal" => Ok(EvaluationStatus::TERMINAL.to_vec()),
        name => EvaluationStatus::iter()
            .find(|s| format!("{s:?}").eq_ignore_ascii_case(name))
            .map(|s| vec![s])
            .ok_or_else(|| {
                WebError::from(TaskRefusal::UnknownEvaluationStatus {
                    name: name.to_string(),
                })
            }),
    }
}

/// Every entry point on a page is seeding one dependency-closure walk. A page of 100 over a
/// 74-entry-point NixOS flake measured 93 s a call and 80% of the database's time.
const ENTRY_POINTS_PAGE: u64 = 25;
const ENTRY_POINTS_PAGE_MAX: u64 = 500;

#[derive(Deserialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EntryPointsQuery {
    pub evaluation_id: Option<EvaluationId>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub search: Option<String>,
}

fn page_bounds(limit: Option<u64>, offset: Option<u64>) -> (u64, u64) {
    let limit = match limit {
        Some(0) | None => ENTRY_POINTS_PAGE,
        Some(n) => n.min(ENTRY_POINTS_PAGE_MAX),
    };

    (limit, offset.unwrap_or(0))
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/entry-points",
    operation_id = "listTaskEntryPoints",
    tag = "tasks",
    summary = "List entry points",
    description = "Returns the root builds (entry points) for the task from the last evaluation by default. Pass `evaluation_id` to retrieve entry points from a specific evaluation instead.",
    params(TaskPath, EntryPointsQuery),
    responses((status = OK, description = "Entry points", body = PaginatedEntryPoints), WebError)
)]
pub async fn get_task_entry_points(
    TaskScope { task, db, .. }: TaskScope<Readable>,
    Query(params): Query<EntryPointsQuery>,
) -> WebResult<Json<BaseResponse<PaginatedEntryPoints>>> {
    let eval_id = match params.evaluation_id.or(task.last_evaluation) {
        Some(id) => id,
        None => return Ok(ok_json(PaginatedEntryPoints::default())),
    };

    let evaluation = require::<EEvaluation>(&db.web_db, eval_id).await?;

    if evaluation.task != Some(task.id) {
        return Err(WebError::not_found(Resource::Evaluation));
    }

    let failed_attributes: Vec<FailedAttributeSummary> =
        gradient_db::evaluations::failed_attributes::failed_attributes(&db.web_db, eval_id)
            .await?
            .into_iter()
            .map(FailedAttributeSummary::from)
            .collect();
    let (limit, offset) = page_bounds(params.limit, params.offset);
    let search = params.search.as_deref();
    let total = gradient_db::entry_points::count_built(&db.web_db, eval_id, search).await?;
    let entry_points =
        gradient_db::entry_points::built_page(&db.web_db, eval_id, search, offset, limit).await?;

    let entry_points = if entry_points.is_empty() {
        Vec::new()
    } else {
        EntryPointRelatedData::load(&db.web_db, &evaluation, &entry_points)
            .await?
            .build_summaries(&entry_points)
    };

    Ok(ok_json(PaginatedEntryPoints {
        entry_points,
        total,
        failed_attributes,
    }))
}

struct EntryPointRelatedData {
    shared_builds: HashMap<DerivationId, MDerivationBuild>,
    with_qos: HashSet<DerivationBuildId>,
    build_jobs: HashMap<DerivationId, BuildJobId>,
    derivations: HashMap<DerivationId, MDerivation>,
    has_products: HashMap<DerivationId, bool>,
    outputs: HashMap<DerivationId, BTreeMap<String, String>>,
    attempts: HashMap<DerivationId, MBuildAttempt>,
    deps: HashMap<EntryPointId, BuildStatusCounts>,
    deps_total: HashMap<EntryPointId, i64>,
}

fn output_paths_by_derivation(
    rows: &[MDerivationOutput],
) -> HashMap<DerivationId, BTreeMap<String, String>> {
    let mut out: HashMap<DerivationId, BTreeMap<String, String>> = HashMap::new();

    for row in rows {
        if row.hash == UNKNOWN_OUTPUT_HASH {
            continue;
        }
        out.entry(row.derivation)
            .or_default()
            .insert(row.name.clone(), StorePath::from(row.clone()).full());
    }

    out
}

impl EntryPointRelatedData {
    async fn load(
        web_db: &WebDb,
        evaluation: &MEvaluation,
        entry_points: &[MEntryPoint],
    ) -> WebResult<Self> {
        let db = web_db;
        let eval_id = evaluation.id;
        let drv_ids: Vec<DerivationId> = entry_points.iter().map(|ep| ep.derivation).collect();

        let derivations = gradient_db::graph::reachability::derivations_by_id(db, &drv_ids).await?;
        let shared_builds =
            gradient_db::graph::reachability::shared_builds_by_derivation(db, &drv_ids).await?;

        let build_jobs: HashMap<DerivationId, BuildJobId> =
            gradient_db::build_jobs::of_derivations_in_evaluation(db, eval_id, &drv_ids)
                .await?
                .into_iter()
                .map(|j| (j.derivation, j.id))
                .collect();

        let completed_drv_ids: HashSet<DerivationId> = shared_builds
            .values()
            .filter(|a| a.status == BuildStatus::Completed || a.status == BuildStatus::Substituted)
            .map(|a| a.derivation)
            .collect();

        let output_rows = gradient_db::derivation_outputs::of_derivations(db, &drv_ids).await?;

        let outputs = output_paths_by_derivation(&output_rows);

        let has_products: HashMap<DerivationId, bool> = {
            let built: Vec<&MDerivationOutput> = output_rows
                .iter()
                .filter(|o| completed_drv_ids.contains(&o.derivation))
                .collect();
            let output_ids: Vec<DerivationOutputId> = built.iter().map(|o| o.id).collect();
            let mut m: HashMap<DerivationId, bool> = HashMap::new();
            if !output_ids.is_empty() {
                let products = gradient_db::build_products::of_outputs(db, &output_ids).await?;
                for bp in products {
                    if let Some(output) = built.iter().find(|o| o.id == bp.derivation_output) {
                        m.insert(output.derivation, true);
                    }
                }
            }
            m
        };

        let shared_build_ids: Vec<DerivationBuildId> =
            shared_builds.values().map(|a| a.id).collect();
        let with_qos =
            gradient_db::scheduling::priority::shared_builds_with_qos(db, &shared_build_ids)
                .await?;

        let attempts: HashMap<DerivationId, MBuildAttempt> = {
            let mut by_shared_build =
                gradient_db::scheduling::build_attempt::latest_attempts(db, &shared_build_ids)
                    .await?;
            shared_builds
                .iter()
                .filter_map(|(drv, a)| by_shared_build.remove(&a.id).map(|att| (*drv, att)))
                .collect()
        };

        let raw = gradient_db::task_board::dep_counts::cached_entry_point_dep_counts(
            db,
            evaluation,
            entry_points,
        )
        .await?;
        let mut deps = HashMap::new();
        let mut deps_total = HashMap::new();
        for (ep, per_status) in raw {
            let mut counts = BuildStatusCounts::default();
            let mut total = 0;
            for (status, n) in per_status {
                counts.add(status, n);
                total += n;
            }

            deps.insert(ep, counts);
            deps_total.insert(ep, total);
        }

        Ok(Self {
            shared_builds,
            with_qos,
            build_jobs,
            derivations,
            has_products,
            outputs,
            attempts,
            deps,
            deps_total,
        })
    }

    fn build_summaries(&self, entry_points: &[MEntryPoint]) -> Vec<EntryPointSummary> {
        let mut summaries = Vec::new();
        for ep in entry_points {
            let Some(&build_id) = self.build_jobs.get(&ep.derivation) else {
                continue;
            };
            let Some(drv) = self.derivations.get(&ep.derivation) else {
                continue;
            };
            let build_status = self
                .shared_builds
                .get(&ep.derivation)
                .map(|a| a.status)
                .unwrap_or(BuildStatus::Queued)
                .for_api();
            let attempt = self.attempts.get(&ep.derivation);
            summaries.push(EntryPointSummary {
                id: ep.id,
                build_id,
                derivation_path: drv.drv_path(),
                eval: ep.eval.clone(),
                build_status,
                has_artefacts: *self.has_products.get(&ep.derivation).unwrap_or(&false),
                outputs: self
                    .outputs
                    .get(&ep.derivation)
                    .cloned()
                    .unwrap_or_default(),
                architecture: drv.architecture.clone(),
                build_time_ms: attempt.and_then(|a| a.duration_ms()),
                build_started_at: attempt.and_then(|a| a.build_started_at),
                deps: self.deps.get(&ep.id).copied().unwrap_or_default(),
                deps_total: self.deps_total.get(&ep.id).copied().unwrap_or(0),
                prioritized: self
                    .shared_builds
                    .get(&ep.derivation)
                    .is_some_and(|a| self.with_qos.contains(&a.id)),
                ifd: drv.ifd,
                created_at: ep.created_at,
            });
        }
        summaries
    }
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EntryPointDownloadQuery {
    pub eval: String,
    pub filename: String,
    pub token: Option<String>,
}

async fn serve_hydra_artifact(
    web_db: &WebDb,
    nar_storage: &NarStore,
    build_outputs: Vec<MDerivationOutput>,
    filename: &str,
) -> WebResult<Option<Response>> {
    let output_ids: Vec<DerivationOutputId> = build_outputs.iter().map(|o| o.id).collect();
    if output_ids.is_empty() {
        return Ok(None);
    }

    let rows = match gradient_db::build_products::of_outputs(web_db, &output_ids).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "failed to query build_product rows for artifact serve");
            return Ok(None);
        }
    };

    for product in rows {
        let product_name = &product.name;
        let path_basename = std::path::Path::new(&product.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if product_name != filename && path_basename != filename {
            continue;
        }

        let output = build_outputs
            .iter()
            .find(|o| o.id == product.derivation_output);
        let output_root = match output {
            Some(o) => StorePath::from(o.clone()).full(),
            None => {
                tracing::warn!(%filename, "build_product references unknown output");
                continue;
            }
        };

        let hash = output.map(|o| o.hash.as_str()).unwrap_or("");
        if hash.is_empty() {
            continue;
        }

        let prefix = format!("{}/", output_root);
        let rel = product
            .path
            .strip_prefix(&prefix)
            .map(str::to_owned)
            .unwrap_or_else(|| product.path.trim_start_matches('/').to_owned());

        let (_size, stream) = match nar_storage.get_stream(hash).await {
            Ok(Some(s)) => s,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(output_path = %output_root, error = %e, "Failed to fetch NAR from nar_storage");
                continue;
            }
        };

        match extract_path_from_reader(nar_reader_from_stream(stream), &rel).await {
            Ok(Extracted::File { contents, .. }) => {
                return Ok(Some(
                    (
                        StatusCode::OK,
                        build_product_headers(filename, &product.subtype),
                        contents,
                    )
                        .into_response(),
                ));
            }
            Ok(Extracted::Directory { tar_zst }) => {
                let archive_name = format!("{}.tar.zst", filename);
                return Ok(Some(
                    (StatusCode::OK, archive_headers(&archive_name), tar_zst).into_response(),
                ));
            }
            Err(ExtractError::NotFound) => continue,
            Err(e) => {
                tracing::error!(output_path = %output_root, %rel, error = %e, "Failed to extract path from NAR");
                return Err(anyhow::anyhow!("Failed to extract path from NAR").into());
            }
        }
    }

    Ok(None)
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/entry-point-downloads",
    operation_id = "downloadEntryPointOutput",
    tag = "tasks",
    summary = "Download newest-commit entry point build output",
    description = "Serves the named file from `nix-support/hydra-build-products` for the build of\nthe given entry point in the task's **newest-commit evaluation**\n(`task.last_evaluation`). Designed as a stable permalink - retriggering an\nolder commit does not shadow the latest one. Returns `404` if no evaluation\nhas been recorded yet, or if the entry point / file is missing from the\nnewest evaluation.\n\nAuthentication:\n- **Public projects** - no credentials required.\n- **Private projects** - supply `?token=GRADxxxx` (API key) or a JWT, **or**\n  authenticate via `Authorization: Bearer` header / `jwt_token` session cookie.\n\nThe `eval` value may contain double-quoted Nix attribute segments\n(e.g. `packages.\"x86_64-linux\".hello`). URL-encode `\"` as `%22`.",
    params(("project" = String, Path), ("task" = String, Path), EntryPointDownloadQuery),
    responses((status = OK, description = "File contents with appropriate `Content-Type` and `Content-Disposition` headers.", body = Binary, content_type = "application/octet-stream"), WebError)
)]
pub async fn get_entry_point_download(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(crate::client_ip::ClientIp(client_ip)): Extension<crate::client_ip::ClientIp>,
    Path((project, task)): Path<(String, String)>,
    Query(params): Query<EntryPointDownloadQuery>,
) -> Result<Response, WebError> {
    let project = get_any_project_by_name(&state.db(), project)
        .await?
        .or_not_found(Resource::Project)?;

    let task = gradient_db::tasks::by_name(&state.web_db, project.id, &task)
        .await?
        .or_not_found(Resource::Task)?;

    let (resolved_user, resolved_key) = if let Some(token_str) = params.token {
        let decoded = crate::authorization::decode_jwt(
            &state.web_db,
            &state.jwt_secret,
            &state.memory.last_used_stamps,
            token_str,
        )
        .await
        .map_err(|_| SharedRefusal::InvalidToken)?;
        if let Some(ctx) = decoded.api_key_context()
            && !crate::ip_allowlist::is_allowed(client_ip, &ctx.allowed_ips)
        {
            return Err(SharedRefusal::SourceIpNotAllowed.into());
        }
        let user = by_id::<EUser, _>(&state.web_db, decoded.user_id()).await?;
        (user, decoded.api_key_context().cloned())
    } else {
        (maybe_user, api_key.as_ref().cloned())
    };

    if !project.public {
        match resolved_user {
            Some(ref user) => {
                if !is_project_member(&state.web_db, user.id, project.id, resolved_key.as_ref())
                    .await?
                {
                    return Err(WebError::not_found(Resource::Task));
                }
            }
            None => return Err(SharedRefusal::AuthorizationRequired.into()),
        }
    }

    let evaluation_id = task.last_evaluation.or_not_found(Resource::Evaluation)?;
    let evaluation = require::<EEvaluation>(&state.web_db, evaluation_id).await?;

    let ep = gradient_db::entry_points::by_attribute(&state.web_db, evaluation.id, &params.eval)
        .await?
        .or_not_found(Resource::EntryPoint)?;

    let shared_build = gradient_db::derivation_builds::of_derivation(&state.web_db, ep.derivation)
        .await?
        .or_not_found(Resource::Build)?;

    if shared_build.status != BuildStatus::Completed
        && shared_build.status != BuildStatus::Substituted
    {
        return Err(WebError::not_found(Resource::File));
    }

    let build_outputs =
        gradient_db::derivation_outputs::of_derivation(&state.web_db, ep.derivation).await?;

    match serve_hydra_artifact(
        &state.web_db,
        &state.nar_storage,
        build_outputs,
        &params.filename,
    )
    .await?
    {
        Some(response) => Ok(response),
        None => Err(WebError::not_found(Resource::File)),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn first_line_truncated_takes_first_line_and_caps_length() {
        assert_eq!(super::first_line_truncated("", 100), None);
        assert_eq!(
            super::first_line_truncated("   \n x", 100).as_deref(),
            Some("x")
        );
        assert_eq!(
            super::first_line_truncated("hello world\nsecond", 100).as_deref(),
            Some("hello world")
        );
        let long: String = "a".repeat(250);
        assert_eq!(
            super::first_line_truncated(&long, 100)
                .unwrap()
                .chars()
                .count(),
            100
        );
    }
}

#[cfg(test)]
mod page_tests {
    use super::{ENTRY_POINTS_PAGE, ENTRY_POINTS_PAGE_MAX, page_bounds};

    #[test]
    fn the_page_defaults_and_clamps() {
        assert_eq!(page_bounds(None, None), (ENTRY_POINTS_PAGE, 0));
        assert_eq!(page_bounds(Some(0), Some(30)), (ENTRY_POINTS_PAGE, 30));
        assert_eq!(page_bounds(Some(40), None), (40, 0));
        assert_eq!(
            page_bounds(Some(10_000), Some(5)),
            (ENTRY_POINTS_PAGE_MAX, 5)
        );
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use gradient_entity::ids::DerivationOutputId;

    fn output_row(drv: DerivationId, name: &str, hash: &str, package: &str) -> MDerivationOutput {
        MDerivationOutput {
            id: DerivationOutputId::now_v7(),
            derivation: drv,
            name: name.into(),
            hash: hash.into(),
            package: package.into(),
            ..Default::default()
        }
    }

    #[test]
    fn output_paths_are_absolute_store_paths_grouped_per_derivation() {
        let a = DerivationId::now_v7();
        let b = DerivationId::now_v7();
        let rows = vec![
            output_row(a, "out", "aaaa", "hello-1.0"),
            output_row(a, "dev", "bbbb", "hello-1.0-dev"),
            output_row(b, "out", "cccc", "world-2.0"),
        ];

        let grouped = output_paths_by_derivation(&rows);

        assert_eq!(grouped[&a]["out"], "/nix/store/aaaa-hello-1.0");
        assert_eq!(grouped[&a]["dev"], "/nix/store/bbbb-hello-1.0-dev");
        assert_eq!(grouped[&b]["out"], "/nix/store/cccc-world-2.0");
        assert_eq!(grouped.len(), 2);
    }

    #[test]
    fn unresolved_output_paths_are_omitted() {
        let drv = DerivationId::now_v7();
        let rows = vec![
            output_row(drv, "out", UNKNOWN_OUTPUT_HASH, "hello"),
            output_row(drv, "dev", "bbbb", "hello-dev"),
        ];

        let grouped = output_paths_by_derivation(&rows);

        assert!(!grouped[&drv].contains_key("out"));
        assert_eq!(grouped[&drv]["dev"], "/nix/store/bbbb-hello-dev");
    }

    #[test]
    fn derivation_with_no_resolvable_output_is_absent_entirely() {
        let drv = DerivationId::now_v7();
        let rows = vec![output_row(drv, "out", UNKNOWN_OUTPUT_HASH, "hello")];

        assert!(output_paths_by_derivation(&rows).is_empty());
    }

    #[test]
    fn attr_filter_accepts_a_concrete_path() {
        assert!(parse_attr_filter("packages.x86_64-linux.hello").is_ok());
        assert!(parse_attr_filter(r#"packages."x86_64-linux".hello"#).is_ok());
    }

    #[test]
    fn attr_filter_rejects_pattern_syntax() {
        for bad in ["", "packages.*.hello", "packages.x86_64-linux.#", "a.b,c.d"] {
            assert!(parse_attr_filter(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn attr_filter_allows_a_quoted_star_segment() {
        assert!(parse_attr_filter(r#"packages.x86_64-linux."*""#).is_ok());
    }

    #[test]
    fn status_filter_expands_the_named_groups() {
        assert_eq!(
            parse_status_filter("active").unwrap(),
            EvaluationStatus::ACTIVE.to_vec()
        );
        assert_eq!(
            parse_status_filter("terminal").unwrap(),
            EvaluationStatus::TERMINAL.to_vec()
        );
    }

    #[test]
    fn status_filter_takes_one_status_by_name_case_insensitively() {
        assert_eq!(
            parse_status_filter("building").unwrap(),
            vec![EvaluationStatus::Building]
        );
        assert_eq!(
            parse_status_filter("Completed").unwrap(),
            vec![EvaluationStatus::Completed]
        );
    }

    #[test]
    fn status_filter_rejects_an_unknown_name() {
        assert!(parse_status_filter("Exploded").is_err());
    }
}
