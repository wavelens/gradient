/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    BuildStatusCounts, EntryPointSummary, EvaluationSummary, EvaluationTriggerSummary,
    QueueSummary, TaskDetailsResponse,
};
use crate::access::{Caller, TaskAccess, has_permission, is_project_member, load_task};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::content_type_for_filename;
use crate::error::{ErrorCode, WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::Permission;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::get_any_project_by_name;
use gradient_entity::build::BuildStatus;
use gradient_entity::derivation_output::UNKNOWN_OUTPUT_HASH;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_sources::{check_task_updates, get_commit_info, get_path_from_derivation_output};
use gradient_storage::nar_extract::{
    ExtractError, Extracted, extract_path_from_reader, nar_reader_from_stream,
};
use gradient_types::input::{hex_to_vec, vec_to_hex};
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, Iterable, QueryFilter, QueryOrder, QuerySelect};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

#[derive(Deserialize, Default)]
pub struct EvaluateRequest {
    /// Optional mode controlling how the evaluation is triggered.
    /// `"restart_failed"` skips fetch+eval and re-queues failed builds from the
    /// most recent evaluation. Omit or `null` for a normal evaluation.
    pub mode: Option<String>,
}

/// Builds one [`EvaluationSummary`] per evaluation using grouped DB rollups
/// (status counts, message counts) plus chunked lookups for triggers, commits,
/// and the triggering user - a fixed number of round-trips regardless of size.
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

    let status_counts = gradient_db::build_status_counts_by_evaluation(db, &eval_ids).await?;
    let message_counts = gradient_db::evaluation_message_counts(db, &eval_ids).await?;

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

        // PR number lives in `source_comment` for every PR trigger, or in an
        // approval `waiting_reason` for gated PRs; both expose it as raw JSON.
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
            created_at: evaluation.created_at,
            updated_at: evaluation.updated_at,
        });
    }
    Ok(out)
}

/// `last_check_at` uses `NULL_TIME` as a "re-check immediately" sentinel;
/// surface that as `None` instead of an epoch timestamp.
fn checked_at(t: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    (t != *gradient_types::NULL_TIME).then_some(t)
}

/// First non-blank line of `s`, trimmed, truncated to `max` chars; `None` when
/// `s` has no non-blank line.
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

    let mode = body.as_ref().and_then(|b| b.mode.as_deref());

    if mode == Some("restart_failed") {
        let eval = gradient_ci::trigger_restart_builds(&state.web_db, &task)
            .await
            .map_err(|e| match e {
                gradient_ci::TriggerError::AlreadyInProgress => {
                    WebError::bad_request("Evaluation already in progress")
                }
                gradient_ci::TriggerError::NoPreviousEvaluation => {
                    WebError::bad_request("No previous evaluation to restart from")
                }
                gradient_ci::TriggerError::Db(db_err) => WebError::from(db_err),
            })?;

        return Ok(ok_json(eval.id.to_string()));
    }

    let mut task_for_check = task.clone();
    task_for_check.force_evaluation = true;
    let (_has_updates, commit_hash) = check_task_updates(&state.db(), &task_for_check, None)
        .await
        .map_err(|e| {
            WebError::bad_request_with(
                ErrorCode::REPOSITORY_UNREACHABLE,
                format!("Failed to fetch repository state: {}", e),
            )
        })?;

    let (commit_message, _email, author_name) = get_commit_info(&state.db(), &task, &commit_hash)
        .await
        .unwrap_or_else(|_| (String::new(), None, String::new()));

    // A manual evaluation also bumps tracked flake inputs (OpenPr action).
    // Self-gated: no-ops unless the task qualifies.
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

    let eval = gradient_ci::trigger_evaluation(
        &state.web_db,
        &task,
        commit_hash,
        Some(commit_message),
        Some(author_name),
        None,
        false,
        None,
        None,
        None,
        Some(user.id),
    )
    .await
    .map_err(|e| match e {
        gradient_ci::TriggerError::AlreadyInProgress => {
            WebError::bad_request("Evaluation already in progress")
        }
        gradient_ci::TriggerError::NoPreviousEvaluation => WebError::internal("Unexpected error"),
        gradient_ci::TriggerError::Db(db_err) => WebError::from(db_err),
    })?;

    let eval = gradient_ci::park_if_no_cache(&state.web_db, eval, task.project).await?;
    let eval = gradient_ci::park_if_storage_full(
        &state.web_db,
        eval,
        task.project,
        state.config.storage.max_storage_gb,
    )
    .await?;
    let eval = gradient_ci::park_if_no_workers(&state.web_db, eval, task.project).await?;
    gradient_ci::actions::dispatch_evaluation_created(&state.ci(), &eval).await;

    Ok(ok_json(eval.id.to_string()))
}

/// `GET /tasks/{project}/{task}/evaluations`
///
/// Returns the `keep_evaluations` most recent evaluations for the task,
/// newest first. Identical access rules as other task endpoints.
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

    if let Some(commit) = params.commit.as_deref() {
        let hash = hex_to_vec(commit)
            .ok()
            .filter(|h| h.len() == COMMIT_HASH_BYTES)
            .ok_or_else(|| WebError::bad_request("`commit` must be a 40-character hex hash"))?;

        // A fresh `commit` row is written per evaluation, so one hash maps to
        // many ids; resolve them first rather than joining on every scan.
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

    let query = query.order_by_desc(CEvaluation::CreatedAt);

    // Wildcard coverage cannot be expressed in SQL, so an `attr` search reads a
    // bounded window of the SQL-filtered rows and matches them here.
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

    let (building, queued) = gradient_db::task_queue_summary(&state.web_db, task.id).await?;

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
    /// Full 40-character commit hash, hex.
    pub commit: Option<String>,
    /// `active`, `terminal`, or one exact status name (`Building`, `Failed`, ...).
    pub status: Option<String>,
    /// Concrete attribute path. Matches evaluations whose *wildcard* covers it,
    /// which is set when the row is created and so answers at every status -
    /// unlike entry points, which only exist once derivations have resolved.
    pub attr: Option<String>,
}

/// A git commit hash is 40 hex characters, so 20 bytes once decoded.
const COMMIT_HASH_BYTES: usize = 20;

/// How many rows an `attr` search reads before matching in Rust. Evaluations are
/// GC-bounded per task by `keep_evaluations`, so this only truncates a task
/// configured to retain more than this, and only for the oldest of them.
const ATTR_SCAN_LIMIT: u64 = 1000;

/// Validates the `attr` filter as a concrete attribute path: one of the paths a
/// wildcard may cover, carrying no pattern syntax of its own. Segments inside
/// double quotes are left alone so an attribute genuinely named `*` still works.
fn parse_attr_filter(attr: &str) -> WebResult<&str> {
    let unquoted_has = |c: char| attr.split('"').step_by(2).any(|part| part.contains(c));

    if attr.is_empty() || attr.contains(',') || unquoted_has('*') || unquoted_has('#') {
        return Err(WebError::bad_request(
            "`attr` must be a concrete attribute path, without wildcard syntax",
        ));
    }

    Ok(attr)
}

/// Resolves the `status` filter to the set of statuses it names.
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

#[derive(Deserialize, Debug)]
pub struct EntryPointsQuery {
    pub evaluation_id: Option<EvaluationId>,
}

pub async fn get_task_entry_points(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
    Query(params): Query<EntryPointsQuery>,
) -> WebResult<Json<BaseResponse<Vec<EntryPointSummary>>>> {
    let (_project, task) = load_task(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Readable,
    )
    .await?;

    // Use the requested evaluation ID, or fall back to the task's last evaluation.
    let eval_id = match params.evaluation_id.or(task.last_evaluation) {
        Some(id) => id,
        None => {
            return Ok(ok_json(vec![]));
        }
    };

    let evaluation = EEvaluation::find_by_id(eval_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Evaluation")?;

    if evaluation.task != Some(task.id) {
        return Err(WebError::not_found("Evaluation"));
    }

    let entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(eval_id))
        .all(&state.web_db)
        .await?;

    if entry_points.is_empty() {
        return Ok(ok_json(vec![]));
    }

    let data = EntryPointRelatedData::load(&state, &entry_points).await?;
    let summaries = data.build_summaries(&entry_points);

    Ok(ok_json(summaries))
}

// ── Entry-point bulk data loader ─────────────────────────────────────────────

/// All DB data needed to render a list of [`EntryPointSummary`] records.
///
/// Loaded in one pass via `load` to avoid per-entry-point round-trips. Keyed on
/// the entry point's derivation; the shared `derivation_build` anchor carries
/// status and the per-eval `build_job` carries the public build id.
struct EntryPointRelatedData {
    anchors: HashMap<DerivationId, MDerivationBuild>,
    build_jobs: HashMap<DerivationId, BuildJobId>,
    derivations: HashMap<DerivationId, MDerivation>,
    has_products: HashMap<DerivationId, bool>,
    outputs: HashMap<DerivationId, BTreeMap<String, String>>,
    build_time_ms: HashMap<DerivationId, Option<i64>>,
    deps: HashMap<EntryPointId, BuildStatusCounts>,
}

/// Groups output rows into `output name -> full /nix/store path` per derivation.
/// Rows whose store path the evaluator never resolved carry the
/// [`UNKNOWN_OUTPUT_HASH`] sentinel and are dropped: there is no path to report.
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
    async fn load(state: &Arc<ServerState>, entry_points: &[MEntryPoint]) -> WebResult<Self> {
        let db = &state.web_db;
        let eval_id = entry_points[0].evaluation;
        let drv_ids: Vec<DerivationId> = entry_points.iter().map(|ep| ep.derivation).collect();

        // Heal any entry-point closure materialised empty mid-eval - before its
        // dependency edges flushed - and frozen at zero, so `deps_total` below
        // reflects the real closure instead of showing a single dep while the
        // graph page is correct. Cheap in steady state: only NULL/zero-count
        // roots recompute; positive counts are trusted and skipped.
        let healed = match gradient_db::materialize_entry_point_closures(db, eval_id).await {
            Ok(n) => n > 0,
            Err(e) => {
                tracing::warn!(evaluation_id = %eval_id, error = %e, "entry-point closure heal failed");
                false
            }
        };

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

        let anchors: HashMap<DerivationId, MDerivationBuild> =
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

        let completed_drv_ids: HashSet<DerivationId> = anchors
            .values()
            .filter(|a| a.status == BuildStatus::Completed || a.status == BuildStatus::Substituted)
            .map(|a| a.derivation)
            .collect();

        // Output rows are written at eval time from the resolved `.drv`, so they
        // exist regardless of build status; `build_status` is what tells a caller
        // whether the path is realised. `hash` falls back to the literal
        // "unknown" when the evaluator could not parse the output path (floating
        // CA outputs), and nothing ever rewrites it - skip those rather than
        // hand out a path that cannot exist.
        let output_rows = gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
            EDerivationOutput::find()
                .filter(CDerivationOutput::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await?;

        let outputs = output_paths_by_derivation(&output_rows);

        // Determine which derivations have at least one build_product. Only
        // built derivations can have products, so the product lookup stays
        // scoped to their outputs.
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
                    // Map back from output → derivation.
                    if let Some(output) = built.iter().find(|o| o.id == bp.derivation_output) {
                        m.insert(output.derivation, true);
                    }
                }
            }
            m
        };

        // Latest attempt per anchor, batched into one DISTINCT ON query, then
        // re-keyed by derivation for the summary lookup.
        let build_time_ms: HashMap<DerivationId, Option<i64>> = {
            let anchor_ids: Vec<DerivationBuildId> = anchors.values().map(|a| a.id).collect();
            let by_anchor = gradient_db::latest_attempts(db, &anchor_ids).await?;
            anchors
                .iter()
                .filter_map(|(drv, a)| by_anchor.get(&a.id).map(|att| (*drv, att.duration_ms())))
                .collect()
        };

        // Read the incrementally-maintained per-entry-point counts (#383). Evals
        // predating that machinery have no rows. Backfill them once (a single
        // closure recompute that persists the counts) instead of running the
        // live closure CTE on every request, which pegged Postgres for ~10s per
        // page load (#391); fall back to the live CTE only if the backfill fails.
        let entry_point_ids: Vec<EntryPointId> = entry_points.iter().map(|ep| ep.id).collect();
        let mut raw = gradient_db::load_entry_point_dep_counts(db, &entry_point_ids).await?;
        // The incremental deltas are only authoritative once an eval finishes and
        // reseeds; mid-eval they miss transitions that fire before a dep's closure
        // edge exists, so a root's counts fall behind its closure (the task page
        // then shows a handful of deps while the graph is correct). Rebuild when a
        // closure just healed, when a historical eval has no counts, or when any
        // root's stored total no longer covers its materialised closure - a cheap
        // recompute over the already-materialised closure that settles after one
        // pass, since `apply_dep_count_delta` preserves the per-root total.
        let histogram_stale = gradient_db::histogram_needs_rebuild(entry_points.iter().map(|ep| {
            let stored: i64 = raw.get(&ep.id).map(|m| m.values().sum()).unwrap_or(0);
            let closure = derivations
                .get(&ep.derivation)
                .and_then(|d| d.dep_closure_count)
                .unwrap_or(0);
            (stored, closure)
        }));
        if healed || raw.is_empty() || histogram_stale {
            match gradient_db::init_entry_point_dep_counts(db, eval_id).await {
                Ok(()) => {
                    raw = gradient_db::load_entry_point_dep_counts(db, &entry_point_ids).await?;
                }
                Err(e) => {
                    tracing::warn!(evaluation_id = %eval_id, error = %e,
                        "dep-count rebuild failed; using live closure CTE");
                    let seeds: Vec<(EntryPointId, uuid::Uuid)> = entry_points
                        .iter()
                        .map(|ep| (ep.id, ep.derivation.into_inner()))
                        .collect();
                    raw = gradient_db::entry_point_dep_counts(db, eval_id, &seeds).await?;
                }
            }
        }
        let deps: HashMap<EntryPointId, BuildStatusCounts> = raw
            .into_iter()
            .map(|(ep, per_status)| {
                let mut c = BuildStatusCounts::default();
                for (status, n) in per_status {
                    c.add(status, n);
                }
                (ep, c)
            })
            .collect();

        Ok(Self {
            anchors,
            build_jobs,
            derivations,
            has_products,
            outputs,
            build_time_ms,
            deps,
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
                .anchors
                .get(&ep.derivation)
                .map(|a| a.status)
                .unwrap_or(BuildStatus::Queued)
                .for_api();
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
                build_time_ms: self.build_time_ms.get(&ep.derivation).copied().flatten(),
                deps: self.deps.get(&ep.id).copied().unwrap_or_default(),
                deps_total: drv.dep_closure_count,
                created_at: ep.created_at,
            });
        }
        summaries
    }
}

// ── Entry-point download (stable permalink) ──────────────────────────────────

#[derive(Deserialize)]
pub struct EntryPointDownloadQuery {
    /// Nix attribute path of the entry point, e.g. `packages."x86_64-linux".hello`.
    /// URL-encode `"` as `%22` when constructing static links.
    pub eval: String,
    /// Filename listed in `nix-support/hydra-build-products`.
    pub filename: String,
    /// API key (`GRADxxxx`) or JWT.  Required when the owning project is private.
    /// Pass via this parameter for static/permalink URLs; omit if you already have a
    /// session cookie or `Authorization: Bearer` header.
    pub token: Option<String>,
}

/// Look up `build_product` rows for the given outputs, find the one whose
/// `name` matches `filename`, and stream its bytes from `nar_storage`.
///
/// Returns `None` when no matching product is found.
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

        let disposition = if product.subtype == "html" {
            "inline".to_string()
        } else {
            format!("attachment; filename=\"{}\"", filename)
        };

        match extract_path_from_reader(nar_reader_from_stream(stream), &rel).await {
            Ok(Extracted::File { contents, .. }) => {
                return Ok(Some(
                    (
                        StatusCode::OK,
                        [
                            (header::CONTENT_TYPE, content_type_for_filename(filename)),
                            (header::CONTENT_DISPOSITION, disposition.as_str()),
                        ],
                        contents,
                    )
                        .into_response(),
                ));
            }
            Ok(Extracted::Directory { tar_zst }) => {
                let archive_name = format!("{}.tar.zst", filename);
                return Ok(Some(
                    (
                        StatusCode::OK,
                        [
                            (header::CONTENT_TYPE, "application/zstd"),
                            (
                                header::CONTENT_DISPOSITION,
                                &format!("attachment; filename=\"{}\"", archive_name),
                            ),
                        ],
                        tar_zst,
                    )
                        .into_response(),
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

/// Downloads the build output for a specific entry point from the task's
/// newest-commit evaluation (`task.last_evaluation`), finds the entry point
/// matching `eval`, and serves the named file from `nix-support/hydra-build-products`.
///
/// Authentication:
/// - Public projects: no credentials required.
/// - Private projects: supply `?token=GRADxxxx` (API key) or a JWT, **or** authenticate
///   via the `Authorization: Bearer` header / `jwt_token` session cookie.
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

    // Resolve caller identity from ?token= (API key / JWT) or existing session.
    // When a token is supplied it provides its own ApiKeyContext; otherwise the
    // middleware-supplied extension applies.
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

    // Newest-commit evaluation - `last_evaluation` over a query avoids a stale
    // completed run shadowing the latest one (#185).
    let evaluation_id = task.last_evaluation.or_not_found("Evaluation")?;
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Evaluation")?;

    // Entry point whose `eval` attribute path matches the query param.
    // Axum URL-decodes the value automatically, so %22 → " before this comparison.
    let ep = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation.id))
        .filter(CEntryPoint::Eval.eq(&params.eval))
        .one(&state.web_db)
        .await?
        .or_not_found("Entry point")?;

    let anchor = EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.eq(ep.derivation))
        .one(&state.web_db)
        .await?
        .or_not_found("Build")?;

    if anchor.status != BuildStatus::Completed && anchor.status != BuildStatus::Substituted {
        return Err(WebError::not_found("File"));
    }

    // Walk derivation outputs, locate the file via hydra-build-products.
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

    /// The evaluator writes the sentinel when an output has no resolvable path
    /// and nothing ever rewrites it, so reporting one would hand a deployment
    /// tool `/nix/store/unknown-...`, which cannot exist.
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

    /// A `*` inside quotes is an attribute literally named `*`, not a pattern.
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
