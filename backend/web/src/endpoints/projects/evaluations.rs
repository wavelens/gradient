/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    EntryPointSummary, EvaluationSummary, EvaluationTriggerSummary, ProjectDetailsResponse,
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
use entity::build::BuildStatus;
use gradient_core::db::get_any_organization_by_name;
use gradient_core::sources::{check_project_updates, get_path_from_derivation_output};
use gradient_core::storage::nar_extract::{ExtractError, Extracted, extract_path_from_nar_bytes};
use gradient_core::types::input::vec_to_hex;
use gradient_core::types::*;
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

/// Builds one [`EvaluationSummary`] per evaluation using a fixed number of DB
/// round-trips regardless of input size (commits, builds, entry_points,
/// entry-point builds - 4 queries total).
pub(super) async fn evaluations_to_summaries(
    state: &Arc<ServerState>,
    evaluations: Vec<MEvaluation>,
) -> Result<Vec<EvaluationSummary>, WebError> {
    if evaluations.is_empty() {
        return Ok(Vec::new());
    }

    let eval_ids: Vec<EvaluationId> = evaluations.iter().map(|e| e.id).collect();

    let trigger_ids: Vec<ProjectTriggerId> = evaluations.iter().filter_map(|e| e.trigger).collect();
    let triggers: HashMap<ProjectTriggerId, TriggerType> = if trigger_ids.is_empty() {
        HashMap::new()
    } else {
        EProjectTrigger::find()
            .filter(CProjectTrigger::Id.is_in(trigger_ids))
            .all(&state.web_db)
            .await?
            .into_iter()
            .filter_map(|t| {
                TriggerType::try_from(t.trigger_type)
                    .ok()
                    .map(|tt| (t.id, tt))
            })
            .collect()
    };

    let prev_ids: Vec<EvaluationId> = evaluations.iter().filter_map(|e| e.previous).collect();
    let mut combined_eval_ids: Vec<EvaluationId> = eval_ids.clone();
    combined_eval_ids.extend(prev_ids.iter().copied());
    let commit_ids: Vec<CommitId> = evaluations.iter().map(|e| e.commit).collect();

    let commits: HashMap<CommitId, String> = ECommit::find()
        .filter(CCommit::Id.is_in(commit_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|c| (c.id, vec_to_hex(&c.hash)))
        .collect();

    let mut total_per_eval: HashMap<EvaluationId, i64> = HashMap::new();
    let mut failed_per_eval: HashMap<EvaluationId, i64> = HashMap::new();
    for build in EBuild::find()
        .filter(CBuild::Evaluation.is_in(eval_ids.clone()))
        .all(&state.web_db)
        .await?
    {
        *total_per_eval.entry(build.evaluation).or_insert(0) += 1;
        if matches!(build.status, BuildStatus::FailedPermanent | BuildStatus::FailedTimeout) {
            *failed_per_eval.entry(build.evaluation).or_insert(0) += 1;
        }
    }

    let entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.is_in(combined_eval_ids))
        .all(&state.web_db)
        .await?;

    let ep_build_ids: Vec<BuildId> = entry_points.iter().map(|ep| ep.build).collect();
    let ep_build_status: HashMap<BuildId, BuildStatus> = if ep_build_ids.is_empty() {
        HashMap::new()
    } else {
        EBuild::find()
            .filter(CBuild::Id.is_in(ep_build_ids))
            .all(&state.web_db)
            .await?
            .into_iter()
            .map(|b| (b.id, b.status))
            .collect()
    };

    let mut eps_per_eval: HashMap<EvaluationId, Vec<BuildId>> = HashMap::new();
    for ep in &entry_points {
        eps_per_eval
            .entry(ep.evaluation)
            .or_default()
            .push(ep.build);
    }

    let mut out = Vec::with_capacity(evaluations.len());
    for evaluation in evaluations {
        let commit_hash = commits.get(&evaluation.commit).cloned().unwrap_or_default();
        let total_builds = *total_per_eval.get(&evaluation.id).unwrap_or(&0);
        let failed_builds = *failed_per_eval.get(&evaluation.id).unwrap_or(&0);

        let ep_builds: &[BuildId] = eps_per_eval
            .get(&evaluation.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let total_entry_points = ep_builds.len() as i64;
        let mut completed_entry_points = 0i64;
        let mut failed_entry_points = 0i64;
        for build_id in ep_builds {
            match ep_build_status.get(build_id) {
                Some(BuildStatus::Completed) | Some(BuildStatus::Substituted) => {
                    completed_entry_points += 1;
                }
                Some(BuildStatus::FailedPermanent) | Some(BuildStatus::FailedTimeout) => {
                    failed_entry_points += 1
                }
                _ => {}
            }
        }

        let entry_point_diff = evaluation.previous.map(|prev_id| {
            let prev_count = eps_per_eval.get(&prev_id).map(|v| v.len()).unwrap_or(0) as i64;
            total_entry_points - prev_count
        });

        let trigger = evaluation.trigger.and_then(|tid| {
            triggers.get(&tid).map(|&tt| EvaluationTriggerSummary {
                id: tid,
                trigger_type: tt,
            })
        });

        out.push(EvaluationSummary {
            id: evaluation.id,
            commit: commit_hash,
            status: evaluation.status,
            trigger,
            total_builds,
            failed_builds,
            completed_entry_points,
            failed_entry_points,
            entry_point_diff,
            created_at: evaluation.created_at,
            updated_at: evaluation.updated_at,
        });
    }
    Ok(out)
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
        gradient_core::ci::trigger_restart_builds(&state.web_db, &project)
            .await
            .map_err(|e| match e {
                gradient_core::ci::TriggerError::AlreadyInProgress => {
                    WebError::bad_request("Evaluation already in progress")
                }
                gradient_core::ci::TriggerError::NoPreviousEvaluation => {
                    WebError::bad_request("No previous evaluation to restart from")
                }
                gradient_core::ci::TriggerError::Db(db_err) => WebError::from(db_err),
            })?;

        return Ok(ok_json("Restarting failed builds".to_string()));
    }

    let mut project_for_check = project.clone();
    project_for_check.force_evaluation = true;
    let (_has_updates, commit_hash) =
        check_project_updates(Arc::clone(&state), &project_for_check, None)
            .await
            .map_err(|e| {
                WebError::bad_request_with(
                    ErrorCode::REPOSITORY_UNREACHABLE,
                    format!("Failed to fetch repository state: {}", e),
                )
            })?;

    let eval = gradient_core::ci::trigger_evaluation(
        &state.web_db,
        &project,
        commit_hash,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
    .await
    .map_err(|e| match e {
        gradient_core::ci::TriggerError::AlreadyInProgress => {
            WebError::bad_request("Evaluation already in progress")
        }
        gradient_core::ci::TriggerError::NoPreviousEvaluation => {
            WebError::internal("Unexpected error")
        }
        gradient_core::ci::TriggerError::Db(db_err) => WebError::from(db_err),
    })?;

    let eval =
        gradient_core::ci::park_if_no_cache(&state.web_db, eval, project.organization).await?;
    let eval = gradient_core::ci::park_if_storage_full(
        &state.web_db,
        eval,
        project.organization,
        state.config.storage.max_storage_gb,
    )
    .await?;
    let eval =
        gradient_core::ci::park_if_no_workers(&state.web_db, eval, project.organization).await?;
    gradient_core::ci::actions::dispatch_evaluation_created(&state, &eval).await;

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

    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(project.keep_evaluations as u64)
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

    // Get last 5 evaluations for this project
    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(5)
        .all(&state.web_db)
        .await?;

    let evaluation_summaries = evaluations_to_summaries(&state.0, evaluations).await?;

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
    let summaries = data.build_summaries(&entry_points, &evaluation);

    Ok(ok_json(summaries))
}

// ── Entry-point bulk data loader ─────────────────────────────────────────────

/// All DB data needed to render a list of [`EntryPointSummary`] records.
///
/// Loaded in one pass via `load` to avoid per-entry-point round-trips.
struct EntryPointRelatedData {
    builds: HashMap<BuildId, MBuild>,
    derivations: HashMap<DerivationId, MDerivation>,
    /// Derivation IDs that have at least one `build_product` row.
    has_products: HashMap<DerivationId, bool>,
}

impl EntryPointRelatedData {
    async fn load(state: &Arc<ServerState>, entry_points: &[MEntryPoint]) -> WebResult<Self> {
        let build_ids: Vec<BuildId> = entry_points.iter().map(|ep| ep.build).collect();
        let builds: HashMap<BuildId, MBuild> = EBuild::find()
            .filter(CBuild::Id.is_in(build_ids))
            .all(&state.web_db)
            .await?
            .into_iter()
            .map(|b| (b.id, b))
            .collect();

        let drv_ids: Vec<DerivationId> = builds.values().map(|b| b.derivation).collect();
        let derivations: HashMap<DerivationId, MDerivation> = if drv_ids.is_empty() {
            HashMap::new()
        } else {
            EDerivation::find()
                .filter(CDerivation::Id.is_in(drv_ids.clone()))
                .all(&state.web_db)
                .await?
                .into_iter()
                .map(|d| (d.id, d))
                .collect()
        };

        let completed_drv_ids: Vec<DerivationId> = builds
            .values()
            .filter(|b| b.status == BuildStatus::Completed || b.status == BuildStatus::Substituted)
            .map(|b| b.derivation)
            .collect();

        // Determine which derivations have at least one build_product by looking
        // at their outputs.
        let has_products: HashMap<DerivationId, bool> = if completed_drv_ids.is_empty() {
            HashMap::new()
        } else {
            let outputs = EDerivationOutput::find()
                .filter(CDerivationOutput::Derivation.is_in(completed_drv_ids))
                .all(&state.web_db)
                .await?;
            let output_ids: Vec<DerivationOutputId> = outputs.iter().map(|o| o.id).collect();
            let mut m: HashMap<DerivationId, bool> = HashMap::new();
            if !output_ids.is_empty() {
                for bp in EBuildProduct::find()
                    .filter(CBuildProduct::DerivationOutput.is_in(output_ids))
                    .all(&state.web_db)
                    .await?
                {
                    // Map back from output → derivation.
                    if let Some(output) = outputs.iter().find(|o| o.id == bp.derivation_output) {
                        m.insert(output.derivation, true);
                    }
                }
            }
            m
        };

        Ok(Self {
            builds,
            derivations,
            has_products,
        })
    }

    /// Assemble summary records for `entry_points` using pre-loaded data.
    fn build_summaries(
        &self,
        entry_points: &[MEntryPoint],
        evaluation: &MEvaluation,
    ) -> Vec<EntryPointSummary> {
        let mut summaries = Vec::new();
        for ep in entry_points {
            let Some(build) = self.builds.get(&ep.build) else {
                continue;
            };
            let Some(drv) = self.derivations.get(&build.derivation) else {
                continue;
            };
            summaries.push(EntryPointSummary {
                id: ep.id,
                build_id: build.id,
                derivation_path: drv.store_path(),
                eval: ep.eval.clone(),
                build_status: build.status.for_api(),
                has_artefacts: *self.has_products.get(&build.derivation).unwrap_or(&false),
                architecture: drv.architecture.clone(),
                evaluation_id: evaluation.id,
                evaluation_status: evaluation.status,
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

    let rows = match EBuildProduct::find()
        .filter(CBuildProduct::DerivationOutput.is_in(output_ids))
        .all(&state.web_db)
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
            Some(o) => get_path_from_derivation_output(o.clone()),
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
    let organization = get_any_organization_by_name(state.0.clone(), organization)
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
            && !gradient_core::ip_allowlist::is_allowed(client_ip, &ctx.allowed_ips)
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

    let build = EBuild::find_by_id(ep.build)
        .one(&state.web_db)
        .await?
        .or_not_found("Build")?;

    if build.status != BuildStatus::Completed && build.status != BuildStatus::Substituted {
        return Err(WebError::not_found("File"));
    }

    // Walk derivation outputs, locate the file via hydra-build-products.
    let build_outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(build.derivation))
        .all(&state.web_db)
        .await?;

    match serve_hydra_artifact(&state, build_outputs, &params.filename).await? {
        Some(response) => Ok(response),
        None => Err(WebError::not_found("File")),
    }
}
