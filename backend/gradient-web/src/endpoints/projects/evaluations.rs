/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    BuildStatusCounts, EntryPointSummary, EvaluationSummary, EvaluationTriggerSummary,
    ProjectDetailsResponse, QueueSummary,
};
use crate::access::{Caller, ProjectAccess, has_permission, is_org_member, load_project};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::content_type_for_filename;
use crate::error::{ErrorCode, WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::Permission;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_db::get_any_organization_by_name;
use gradient_sources::{check_project_updates, get_commit_info, get_path_from_derivation_output};
use gradient_storage::nar_extract::{ExtractError, Extracted, extract_path_from_nar_bytes};
use gradient_types::input::vec_to_hex;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::Deserialize;
use std::collections::HashMap;
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

    let trigger_ids: Vec<ProjectTriggerId> =
        evaluations.iter().filter_map(|e| e.trigger).collect();
    let triggers: HashMap<ProjectTriggerId, TriggerType> =
        gradient_db::fetch_in_chunks(&trigger_ids, |chunk| async move {
            EProjectTrigger::find()
                .filter(CProjectTrigger::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .filter_map(|t| TriggerType::try_from(t.trigger_type).ok().map(|tt| (t.id, tt)))
        .collect();

    let commit_ids: Vec<CommitId> = evaluations.iter().map(|e| e.commit).collect();
    let commits: HashMap<CommitId, MCommit> =
        gradient_db::fetch_in_chunks(&commit_ids, |chunk| async move {
            ECommit::find().filter(CCommit::Id.is_in(chunk)).all(db).await
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
        gradient_db::build_status_counts_by_evaluation(db, &eval_ids).await?;
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
            .or_else(|| evaluation.waiting_reason.as_ref().and_then(|v| v.get("pr_number")))
            .and_then(|n| n.as_u64());

        out.push(EvaluationSummary {
            id: evaluation.id,
            commit: commit_hash,
            commit_message,
            status: evaluation.status,
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

pub async fn post_project_evaluate(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
    body: Option<Json<EvaluateRequest>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_organization, project) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    let mode = body.as_ref().and_then(|b| b.mode.as_deref());

    if mode == Some("restart_failed") {
        gradient_ci::trigger_restart_builds(&state.web_db, &project)
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

        return Ok(ok_json("Restarting failed builds".to_string()));
    }

    let mut project_for_check = project.clone();
    project_for_check.force_evaluation = true;
    let (_has_updates, commit_hash) =
        check_project_updates(&state.db(), &project_for_check, None)
            .await
            .map_err(|e| {
                WebError::bad_request_with(
                    ErrorCode::REPOSITORY_UNREACHABLE,
                    format!("Failed to fetch repository state: {}", e),
                )
            })?;

    let (commit_message, _email, author_name) = get_commit_info(&state.db(), &project, &commit_hash)
        .await
        .unwrap_or_else(|_| (String::new(), None, String::new()));

    let eval = gradient_ci::trigger_evaluation(
        &state.web_db,
        &project,
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
        gradient_ci::TriggerError::NoPreviousEvaluation => {
            WebError::internal("Unexpected error")
        }
        gradient_ci::TriggerError::Db(db_err) => WebError::from(db_err),
    })?;

    let eval =
        gradient_ci::park_if_no_cache(&state.web_db, eval, project.organization).await?;
    let eval = gradient_ci::park_if_storage_full(
        &state.web_db,
        eval,
        project.organization,
        state.config.storage.max_storage_gb,
    )
    .await?;
    let eval =
        gradient_ci::park_if_no_workers(&state.web_db, eval, project.organization).await?;
    gradient_ci::actions::dispatch_evaluation_created(&state.ci(), &eval).await;

    Ok(ok_json("Evaluation started".to_string()))
}

/// `GET /projects/{organization}/{project}/evaluations`
///
/// Returns the `keep_evaluations` most recent evaluations for the project,
/// newest first. Identical access rules as other project endpoints.
pub async fn get_project_evaluations(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
    Query(params): Query<EvaluationsQuery>,
) -> WebResult<Json<BaseResponse<Vec<EvaluationSummary>>>> {
    let (_organization, project) = load_project(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Readable,
    )
    .await?;

    let limit = params.limit.unwrap_or(project.keep_evaluations as u64);
    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(limit)
        .all(&state.web_db)
        .await?;

    let summaries = evaluations_to_summaries(&state.0, evaluations).await?;

    Ok(ok_json(summaries))
}

pub async fn get_project_details(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<ProjectDetailsResponse>>> {
    let api_key_ref = api_key.as_ref();
    let (organization, project) = load_project(
        &state,
        Caller::from_option(&maybe_user),
        api_key_ref,
        organization,
        project,
        ProjectAccess::Readable,
    )
    .await?;

    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(10)
        .all(&state.web_db)
        .await?;

    let evaluation_summaries = evaluations_to_summaries(&state.0, evaluations).await?;

    let (building, queued) =
        gradient_db::project_queue_summary(&state.web_db, project.id).await?;

    let (can_edit, can_trigger) = match &maybe_user {
        Some(user) => (
            has_permission(
                &state,
                user.id,
                organization.id,
                Permission::EditProject,
                api_key_ref,
            )
            .await?,
            has_permission(
                &state,
                user.id,
                organization.id,
                Permission::TriggerEvaluation,
                api_key_ref,
            )
            .await?,
        ),
        None => (false, false),
    };

    let project_details = ProjectDetailsResponse {
        id: project.id,
        name: project.name,
        display_name: project.display_name,
        description: project.description,
        repository: project.repository,
        wildcard: project.wildcard,
        active: project.active,
        created_at: project.created_at,
        keep_evaluations: project.keep_evaluations,
        last_check_at: checked_at(project.last_check_at),
        queue: QueueSummary { building, queued },
        last_evaluations: evaluation_summaries,
        can_edit,
        can_trigger,
        managed: project.managed,
    };

    let res = BaseResponse {
        error: false,
        message: project_details,
    };

    Ok(Json(res))
}

#[derive(Deserialize, Debug, Default)]
pub struct EvaluationsQuery {
    pub limit: Option<u64>,
}

#[derive(Deserialize, Debug)]
pub struct EntryPointsQuery {
    pub evaluation_id: Option<EvaluationId>,
}

pub async fn get_project_entry_points(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
    Query(params): Query<EntryPointsQuery>,
) -> WebResult<Json<BaseResponse<Vec<EntryPointSummary>>>> {
    let (_organization, project) = load_project(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Readable,
    )
    .await?;

    // Use the requested evaluation ID, or fall back to the project's last evaluation.
    let eval_id = match params.evaluation_id.or(project.last_evaluation) {
        Some(id) => id,
        None => {
            return Ok(ok_json(vec![]));
        }
    };

    let evaluation = EEvaluation::find_by_id(eval_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Evaluation")?;

    if evaluation.project != Some(project.id) {
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
    build_time_ms: HashMap<DerivationId, Option<i64>>,
    deps: HashMap<EntryPointId, BuildStatusCounts>,
}

impl EntryPointRelatedData {
    async fn load(state: &Arc<ServerState>, entry_points: &[MEntryPoint]) -> WebResult<Self> {
        let db = &state.web_db;
        let eval_id = entry_points[0].evaluation;
        let drv_ids: Vec<DerivationId> = entry_points.iter().map(|ep| ep.derivation).collect();

        let derivations: HashMap<DerivationId, MDerivation> =
            gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
                EDerivation::find().filter(CDerivation::Id.is_in(chunk)).all(db).await
            })
            .await?
            .into_iter()
            .map(|d| (d.id, d))
            .collect();

        let anchors: HashMap<DerivationId, MDerivationBuild> =
            gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
                EDerivationBuild::find().filter(CDerivationBuild::Derivation.is_in(chunk)).all(db).await
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

        let completed_drv_ids: Vec<DerivationId> = anchors
            .values()
            .filter(|a| a.status == BuildStatus::Completed || a.status == BuildStatus::Substituted)
            .map(|a| a.derivation)
            .collect();

        // Determine which derivations have at least one build_product by looking
        // at their outputs.
        let has_products: HashMap<DerivationId, bool> = if completed_drv_ids.is_empty() {
            HashMap::new()
        } else {
            let outputs = gradient_db::fetch_in_chunks(&completed_drv_ids, |chunk| async move {
                EDerivationOutput::find()
                    .filter(CDerivationOutput::Derivation.is_in(chunk))
                    .all(db)
                    .await
            })
            .await?;
            let output_ids: Vec<DerivationOutputId> = outputs.iter().map(|o| o.id).collect();
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
                    if let Some(output) = outputs.iter().find(|o| o.id == bp.derivation_output) {
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
        if raw.is_empty() {
            match gradient_db::reconcile_eval_dep_counts(db, eval_id).await {
                Ok(()) => {
                    raw = gradient_db::load_entry_point_dep_counts(db, &entry_point_ids).await?;
                }
                Err(e) => {
                    tracing::warn!(evaluation_id = %eval_id, error = %e,
                        "dep-count backfill failed; using live closure CTE");
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
    /// API key (`GRADxxxx`) or JWT.  Required when the owning organisation is private.
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

        let compressed = match state.nar_storage.get(hash).await {
            Ok(Some(b)) => b,
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

        match extract_path_from_nar_bytes(compressed, &rel).await {
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

/// Downloads the build output for a specific entry point from the project's
/// newest-commit evaluation (`project.last_evaluation`), finds the entry point
/// matching `eval`, and serves the named file from `nix-support/hydra-build-products`.
///
/// Authentication:
/// - Public organisations: no credentials required.
/// - Private organisations: supply `?token=GRADxxxx` (API key) or a JWT, **or** authenticate
///   via the `Authorization: Bearer` header / `jwt_token` session cookie.
pub async fn get_entry_point_download(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(crate::client_ip::ClientIp(client_ip)): Extension<crate::client_ip::ClientIp>,
    Path((organization, project)): Path<(String, String)>,
    Query(params): Query<EntryPointDownloadQuery>,
) -> Result<Response, WebError> {
    let organization = get_any_organization_by_name(&state.db(), organization)
        .await?
        .or_not_found("Organization")?;

    let project = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(&project))
        .one(&state.web_db)
        .await?
        .or_not_found("Project")?;

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

    if !organization.public {
        match resolved_user {
            Some(ref user) => {
                if !is_org_member(&state, user.id, organization.id, resolved_key.as_ref()).await? {
                    return Err(WebError::not_found("Project"));
                }
            }
            None => return Err(WebError::unauthorized("Authorization required")),
        }
    }

    // Newest-commit evaluation - `last_evaluation` over a query avoids a stale
    // completed run shadowing the latest one (#185).
    let evaluation_id = project.last_evaluation.or_not_found("Evaluation")?;
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
        assert_eq!(super::first_line_truncated("   \n x", 100).as_deref(), Some("x"));
        assert_eq!(
            super::first_line_truncated("hello world\nsecond", 100).as_deref(),
            Some("hello world")
        );
        let long: String = "a".repeat(250);
        assert_eq!(super::first_line_truncated(&long, 100).unwrap().chars().count(), 100);
    }
}
