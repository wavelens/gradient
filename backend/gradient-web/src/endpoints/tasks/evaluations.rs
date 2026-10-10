/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    BuildStatusCounts, EntryPointSummary, EvaluationSummary, EvaluationTriggerSummary,
    FailedAttributeSummary, PaginatedEntryPoints, QueueSummary, TaskDetailsResponse,
};
use crate::access::{Caller, TaskAccess, has_permission, is_project_member, load_task};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::evals::{live_progress, live_progress_status};
use crate::endpoints::{archive_headers, build_product_headers};
use crate::error::{ErrorCode, WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::Permission;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::lookup::get_any_project_by_name;
use gradient_entity::build::BuildStatus;
use gradient_entity::derivation_output::UNKNOWN_OUTPUT_HASH;
use gradient_entity::evaluation::{EvaluationStatus, WalkMode};
use gradient_entity::evaluation_message::MessageLevel;
use gradient_sources::{get_commit_info, get_path_from_derivation_output, head_commit};
use gradient_storage::nar_extract::{
    ExtractError, Extracted, extract_path_from_reader, nar_reader_from_stream,
};
use gradient_types::input::{hex_to_vec, vec_to_hex};
use gradient_types::*;
use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Expr, Query as SeaQuery};
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, Iterable, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Select,
};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

#[derive(Deserialize, Default)]
pub struct EvaluateRequest {
    pub commit: Option<String>,
    pub attr: Option<String>,
    pub walk: Option<WalkMode>,
}

pub(super) async fn evaluations_to_summaries(
    state: &Arc<ServerState>,
    evaluations: Vec<MEvaluation>,
) -> Result<Vec<EvaluationSummary>, WebError> {
    if evaluations.is_empty() {
        return Ok(Vec::new());
    }

    let db = &state.web_db;
    let eval_ids: Vec<EvaluationId> = evaluations.iter().map(|e| e.id).collect();

    let trigger_ids: Vec<TaskTriggerId> = evaluations.iter().filter_map(|e| e.trigger).collect();
    let triggers: HashMap<TaskTriggerId, TriggerType> =
        gradient_db::fetch_in_chunks(&trigger_ids, |chunk| async move {
            ETaskTrigger::find()
                .filter(CTaskTrigger::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .map(|t| (t.id, t.trigger_type))
        .collect();

    let commit_ids: Vec<CommitId> = evaluations.iter().map(|e| e.commit).collect();
    let commits: HashMap<CommitId, MCommit> =
        gradient_db::fetch_in_chunks(&commit_ids, |chunk| async move {
            ECommit::find()
                .filter(CCommit::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();

    let user_ids: Vec<UserId> = evaluations.iter().filter_map(|e| e.started_by).collect();
    let user_names: HashMap<UserId, String> =
        gradient_db::fetch_in_chunks(&user_ids, |chunk| async move {
            EUser::find().filter(CUser::Id.is_in(chunk)).all(db).await
        })
        .await?
        .into_iter()
        .map(|u| (u.id, u.name))
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
            triggers.get(&tid).map(|&tt| EvaluationTriggerSummary {
                id: tid,
                trigger_type: tt,
            })
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
                &state.eval_progress,
                evaluation.id,
                evaluation.status,
                Instant::now(),
            ),
            expected_thunks: expected_thunks.filter(|_| live_progress_status(evaluation.status)),
        });
    }
    Ok(out)
}

fn checked_at(t: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    (t != *gradient_types::NULL_TIME).then_some(t)
}

fn first_line_truncated(s: &str, max: usize) -> Option<String> {
    let line = s.lines().find(|l| !l.trim().is_empty())?.trim();
    Some(line.chars().take(max).collect())
}

pub async fn post_task_evaluate(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
    body: Option<Json<EvaluateRequest>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_project, task) = load_task(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    let walk_mode = body.as_ref().and_then(|b| b.walk).unwrap_or_default();

    let pinned = body
        .as_ref()
        .and_then(|b| b.commit.as_deref())
        .map(|commit| {
            hex_to_vec(commit)
                .ok()
                .filter(|h| h.len() == COMMIT_HASH_BYTES)
                .ok_or_else(|| WebError::bad_request("`commit` must be a 40-character hex hash"))
        })
        .transpose()?;

    let attr = body
        .as_ref()
        .and_then(|b| b.attr.as_deref())
        .map(|a| {
            a.parse::<Wildcard>()
                .map(|w| w.to_string())
                .map_err(|e| WebError::bad_request(format!("Invalid `attr`: {}", e)))
        })
        .transpose()?;

    let pinned_run = pinned.is_some();

    let (commit_hash, commit_message, author_name) = match pinned {
        Some(commit_hash) => {
            let commit = get_commit_info(&state.db(), &task, &commit_hash)
                .await
                .map_err(|e| {
                    WebError::bad_request_with(
                        ErrorCode::REPOSITORY_UNREACHABLE,
                        format!("Commit not found in repository: {}", e),
                    )
                })?;
            (commit_hash, commit.message, commit.author_name)
        }
        None => {
            let head = head_commit(&state.db(), &task, None).await.map_err(|e| {
                WebError::bad_request_with(
                    ErrorCode::REPOSITORY_UNREACHABLE,
                    format!("Failed to fetch repository state: {}", e),
                )
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
            WebError::bad_request("Evaluation already in progress")
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

pub async fn get_task_evaluations(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
    Query(params): Query<EvaluationsQuery>,
) -> WebResult<Json<BaseResponse<Vec<EvaluationSummary>>>> {
    let (_project, task) = load_task(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Readable,
    )
    .await?;

    let limit = params.limit.unwrap_or(task.keep_evaluations as u64);
    let attr = params.attr.as_deref().map(parse_attr_filter).transpose()?;

    let mut query = EEvaluation::find().filter(CEvaluation::Task.eq(task.id));

    if let Some(before) = params.before {
        let cursor = EEvaluation::find_by_id(before)
            .filter(CEvaluation::Task.eq(task.id))
            .one(&state.web_db)
            .await?
            .or_not_found("Evaluation")?;
        query = query.filter(older_than(&cursor));
    }

    if let Some(commit) = params.commit.as_deref() {
        let hash = hex_to_vec(commit)
            .ok()
            .filter(|h| h.len() == COMMIT_HASH_BYTES)
            .ok_or_else(|| WebError::bad_request("`commit` must be a 40-character hex hash"))?;

        let commit_ids: Vec<CommitId> = ECommit::find()
            .filter(CCommit::Hash.eq(hash))
            .all(&state.web_db)
            .await?
            .into_iter()
            .map(|c| c.id)
            .collect();

        if commit_ids.is_empty() {
            return Ok(ok_json(vec![]));
        }

        query = query.filter(CEvaluation::Commit.is_in(commit_ids));
    }

    if let Some(status) = params.status.as_deref() {
        query = query.filter(CEvaluation::Status.is_in(parse_status_filter(status)?));
    }

    let query = query
        .order_by_desc(CEvaluation::CreatedAt)
        .order_by_desc(CEvaluation::Id);

    let evaluations = match attr {
        Some(attr) => query
            .limit(ATTR_SCAN_LIMIT)
            .all(&state.web_db)
            .await?
            .into_iter()
            .filter(|e| {
                e.wildcard
                    .parse::<Wildcard>()
                    .is_ok_and(|w| w.matches(attr))
            })
            .take(limit as usize)
            .collect(),
        None => query.limit(limit).all(&state.web_db).await?,
    };

    let summaries = evaluations_to_summaries(&state.0, evaluations).await?;

    Ok(ok_json(summaries))
}

pub async fn get_task_details(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<TaskDetailsResponse>>> {
    let api_key_ref = api_key.as_ref();
    let (project, task) = load_task(
        &state,
        Caller::from_option(&maybe_user),
        api_key_ref,
        project,
        task,
        TaskAccess::Readable,
    )
    .await?;

    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Task.eq(task.id))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(10)
        .all(&state.web_db)
        .await?;

    let evaluation_summaries = evaluations_to_summaries(&state.0, evaluations).await?;

    let (building, queued) =
        gradient_db::task_board::task_queue_summary(&state.web_db, task.id).await?;

    let (can_edit, can_trigger) = match &maybe_user {
        Some(user) => (
            has_permission(
                &state,
                user.id,
                project.id,
                Permission::EditTask,
                api_key_ref,
            )
            .await?,
            has_permission(
                &state,
                user.id,
                project.id,
                Permission::TriggerEvaluation,
                api_key_ref,
            )
            .await?,
        ),
        None => (false, false),
    };

    let task_details = TaskDetailsResponse {
        id: task.id,
        name: task.name,
        display_name: task.display_name,
        description: task.description,
        repository: task.repository,
        wildcard: task.wildcard,
        active: task.active,
        created_at: task.created_at,
        keep_evaluations: task.keep_evaluations,
        last_check_at: checked_at(task.last_check_at),
        queue: QueueSummary { building, queued },
        last_evaluations: evaluation_summaries,
        can_edit,
        can_trigger,
        managed: task.managed,
    };

    let res = BaseResponse {
        error: false,
        message: task_details,
    };

    Ok(Json(res))
}

#[derive(Deserialize, Debug, Default)]
pub struct EvaluationsQuery {
    pub limit: Option<u64>,
    pub before: Option<EvaluationId>,
    pub commit: Option<String>,
    pub status: Option<String>,
    pub attr: Option<String>,
}

const COMMIT_HASH_BYTES: usize = 20;

fn older_than(cursor: &MEvaluation) -> Condition {
    Condition::any()
        .add(CEvaluation::CreatedAt.lt(cursor.created_at))
        .add(
            Condition::all()
                .add(CEvaluation::CreatedAt.eq(cursor.created_at))
                .add(CEvaluation::Id.lt(cursor.id)),
        )
}

const ATTR_SCAN_LIMIT: u64 = 1000;

fn parse_attr_filter(attr: &str) -> WebResult<&str> {
    let unquoted_has = |c: char| attr.split('"').step_by(2).any(|part| part.contains(c));

    if attr.is_empty() || attr.contains(',') || unquoted_has('*') || unquoted_has('#') {
        return Err(WebError::bad_request(
            "`attr` must be a concrete attribute path, without wildcard syntax",
        ));
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
                WebError::bad_request(format!(
                    "Unknown evaluation status `{name}`; expected `active`, `terminal`, or a status name"
                ))
            }),
    }
}

/// Every entry point on a page is seeding one dependency-closure walk. A page of 100 over a
/// 74-entry-point NixOS flake measured 93 s a call and 80% of the database's time.
const ENTRY_POINTS_PAGE: u64 = 25;
const ENTRY_POINTS_PAGE_MAX: u64 = 500;

#[derive(Deserialize, Debug)]
pub struct EntryPointsQuery {
    pub evaluation_id: Option<EvaluationId>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub search: Option<String>,
}

fn built_entry_points(evaluation: EvaluationId, search: Option<&str>) -> Select<EEntryPoint> {
    let has_build_job = SeaQuery::select()
        .column(CBuildJob::Derivation)
        .from(gradient_entity::build_job::Entity)
        .and_where(CBuildJob::Evaluation.eq(evaluation))
        .to_owned();
    let scope = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation))
        .filter(CEntryPoint::Derivation.in_subquery(has_build_job));

    match search.map(str::trim).filter(|term| !term.is_empty()) {
        Some(term) => scope.filter(
            Expr::col(CEntryPoint::Eval).ilike(gradient_db::dashboard::ilike_contains(term)),
        ),
        None => scope,
    }
}

fn page_bounds(limit: Option<u64>, offset: Option<u64>) -> (u64, u64) {
    let limit = match limit {
        Some(0) | None => ENTRY_POINTS_PAGE,
        Some(n) => n.min(ENTRY_POINTS_PAGE_MAX),
    };

    (limit, offset.unwrap_or(0))
}

pub async fn get_task_entry_points(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
    Query(params): Query<EntryPointsQuery>,
) -> WebResult<Json<BaseResponse<PaginatedEntryPoints>>> {
    let (_project, task) = load_task(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Readable,
    )
    .await?;

    let eval_id = match params.evaluation_id.or(task.last_evaluation) {
        Some(id) => id,
        None => return Ok(ok_json(PaginatedEntryPoints::default())),
    };

    let evaluation = EEvaluation::find_by_id(eval_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Evaluation")?;

    if evaluation.task != Some(task.id) {
        return Err(WebError::not_found("Evaluation"));
    }

    let failed_attributes: Vec<FailedAttributeSummary> =
        gradient_db::evaluations::failed_attributes::failed_attributes(&state.web_db, eval_id)
            .await?
            .into_iter()
            .map(|f| FailedAttributeSummary {
                eval: f.attr,
                message: f.message,
            })
            .collect();
    let (limit, offset) = page_bounds(params.limit, params.offset);
    let scope = built_entry_points(eval_id, params.search.as_deref());
    let total = scope.clone().count(&state.web_db).await?;
    let entry_points = scope
        .order_by_asc(CEntryPoint::Eval)
        .offset(offset)
        .limit(limit)
        .all(&state.web_db)
        .await?;

    if entry_points.is_empty() {
        return Ok(ok_json(PaginatedEntryPoints {
            entry_points: Vec::new(),
            total,
            failed_attributes,
        }));
    }

    let data = EntryPointRelatedData::load(&state, &evaluation, &entry_points).await?;

    Ok(ok_json(PaginatedEntryPoints {
        entry_points: data.build_summaries(&entry_points),
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
        out.entry(row.derivation).or_default().insert(
            row.name.clone(),
            get_path_from_derivation_output(row.clone()).full(),
        );
    }

    out
}

impl EntryPointRelatedData {
    async fn load(
        state: &Arc<ServerState>,
        evaluation: &MEvaluation,
        entry_points: &[MEntryPoint],
    ) -> WebResult<Self> {
        let db = &state.web_db;
        let eval_id = evaluation.id;
        let drv_ids: Vec<DerivationId> = entry_points.iter().map(|ep| ep.derivation).collect();

        let derivations: HashMap<DerivationId, MDerivation> =
            gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
                EDerivation::find()
                    .filter(CDerivation::Id.is_in(chunk))
                    .all(db)
                    .await
            })
            .await?
            .into_iter()
            .map(|d| (d.id, d))
            .collect();

        let shared_builds: HashMap<DerivationId, MDerivationBuild> =
            gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
                EDerivationBuild::find()
                    .filter(CDerivationBuild::Derivation.is_in(chunk))
                    .all(db)
                    .await
            })
            .await?
            .into_iter()
            .map(|a| (a.derivation, a))
            .collect();

        let build_jobs: HashMap<DerivationId, BuildJobId> =
            gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
                EBuildJob::find()
                    .filter(CBuildJob::Evaluation.eq(eval_id))
                    .filter(CBuildJob::Derivation.is_in(chunk))
                    .all(db)
                    .await
            })
            .await?
            .into_iter()
            .map(|j| (j.derivation, j.id))
            .collect();

        let completed_drv_ids: HashSet<DerivationId> = shared_builds
            .values()
            .filter(|a| a.status == BuildStatus::Completed || a.status == BuildStatus::Substituted)
            .map(|a| a.derivation)
            .collect();

        let output_rows = gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
            EDerivationOutput::find()
                .filter(CDerivationOutput::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await?;

        let outputs = output_paths_by_derivation(&output_rows);

        let has_products: HashMap<DerivationId, bool> = {
            let built: Vec<&MDerivationOutput> = output_rows
                .iter()
                .filter(|o| completed_drv_ids.contains(&o.derivation))
                .collect();
            let output_ids: Vec<DerivationOutputId> = built.iter().map(|o| o.id).collect();
            let mut m: HashMap<DerivationId, bool> = HashMap::new();
            if !output_ids.is_empty() {
                let products = gradient_db::fetch_in_chunks(&output_ids, |chunk| async move {
                    EBuildProduct::find()
                        .filter(CBuildProduct::DerivationOutput.is_in(chunk))
                        .all(db)
                        .await
                })
                .await?;
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

#[derive(Deserialize)]
pub struct EntryPointDownloadQuery {
    pub eval: String,
    pub filename: String,
    pub token: Option<String>,
}

async fn serve_hydra_artifact(
    state: &Arc<ServerState>,
    build_outputs: Vec<MDerivationOutput>,
    filename: &str,
) -> WebResult<Option<Response>> {
    let output_ids: Vec<DerivationOutputId> = build_outputs.iter().map(|o| o.id).collect();
    if output_ids.is_empty() {
        return Ok(None);
    }

    let db = &state.web_db;
    let rows = match gradient_db::fetch_in_chunks(&output_ids, |chunk| async move {
        EBuildProduct::find()
            .filter(CBuildProduct::DerivationOutput.is_in(chunk))
            .all(db)
            .await
    })
    .await
    {
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
            Some(o) => get_path_from_derivation_output(o.clone()).full(),
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

        let (_size, stream) = match state.nar_storage.get_stream(hash).await {
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
                return Err(WebError::internal(
                    "Failed to extract path from NAR".to_string(),
                ));
            }
        }
    }

    Ok(None)
}

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
        .or_not_found("Project")?;

    let task = ETask::find()
        .filter(CTask::Project.eq(project.id))
        .filter(CTask::Name.eq(&task))
        .one(&state.web_db)
        .await?
        .or_not_found("Task")?;

    let (resolved_user, resolved_key) = if let Some(token_str) = params.token {
        let decoded = crate::authorization::decode_jwt(State(Arc::clone(&state)), token_str)
            .await
            .map_err(|_| WebError::unauthorized("Invalid token"))?;
        if let Some(ctx) = decoded.api_key_context()
            && !crate::ip_allowlist::is_allowed(client_ip, &ctx.allowed_ips)
        {
            return Err(WebError::forbidden_with(
                crate::error::ErrorCode::FORBIDDEN_SOURCE_IP,
                "API key not allowed from this source IP",
            ));
        }
        let user = EUser::find_by_id(decoded.user_id())
            .one(&state.web_db)
            .await?;
        (user, decoded.api_key_context().cloned())
    } else {
        (maybe_user, api_key.as_ref().cloned())
    };

    if !project.public {
        match resolved_user {
            Some(ref user) => {
                if !is_project_member(&state, user.id, project.id, resolved_key.as_ref()).await? {
                    return Err(WebError::not_found("Task"));
                }
            }
            None => return Err(WebError::unauthorized("Authorization required")),
        }
    }

    let evaluation_id = task.last_evaluation.or_not_found("Evaluation")?;
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Evaluation")?;

    let ep = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation.id))
        .filter(CEntryPoint::Eval.eq(&params.eval))
        .one(&state.web_db)
        .await?
        .or_not_found("Entry point")?;

    let shared_build = EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.eq(ep.derivation))
        .one(&state.web_db)
        .await?
        .or_not_found("Build")?;

    if shared_build.status != BuildStatus::Completed
        && shared_build.status != BuildStatus::Substituted
    {
        return Err(WebError::not_found("File"));
    }

    let build_outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(ep.derivation))
        .all(&state.web_db)
        .await?;

    match serve_hydra_artifact(&state, build_outputs, &params.filename).await? {
        Some(response) => Ok(response),
        None => Err(WebError::not_found("File")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn checked_at_maps_null_time_sentinel_to_none() {
        assert_eq!(super::checked_at(*gradient_types::NULL_TIME), None);
        let t = chrono::NaiveDate::from_ymd_opt(2026, 6, 13)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        assert_eq!(super::checked_at(t), Some(t));
    }

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
    use sea_orm::{DatabaseBackend, QueryTrait};

    fn entry_point_sql(search: Option<&str>) -> String {
        built_entry_points(EvaluationId::now_v7(), search)
            .build(DatabaseBackend::Postgres)
            .to_string()
    }

    #[test]
    fn a_search_term_narrows_entry_points_to_matching_attributes() {
        let sql = entry_point_sql(Some(" Hello "));
        assert!(str::contains(&sql, r#""eval" ILIKE '%Hello%'"#), "{sql}");
    }

    #[test]
    fn a_blank_search_term_keeps_every_entry_point() {
        assert!(!str::contains(&entry_point_sql(Some("  ")), "ILIKE"));
        assert!(!str::contains(&entry_point_sql(None), "ILIKE"));
    }

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
