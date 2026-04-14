/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{EntryPointSummary, EvaluationSummary, ProjectDetailsResponse, user_can_edit};
use crate::authorization::MaybeUser;
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use core::db::{get_any_organization_by_name, get_project_by_name};
use core::sources::check_project_updates;
use core::types::input::vec_to_hex;
use core::types::*;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::fs;
use uuid::Uuid;

/// Build an [`EvaluationSummary`] for a single evaluation row.
pub(super) async fn evaluation_to_summary(
    state: &Arc<ServerState>,
    evaluation: MEvaluation,
) -> Result<EvaluationSummary, WebError> {
    let commit_hash = ECommit::find_by_id(evaluation.commit)
        .one(&state.db)
        .await?
        .map(|c| vec_to_hex(&c.hash))
        .unwrap_or_default();

    let total_builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .count(&state.db)
        .await? as i64;

    let failed_builds = EBuild::find()
        .filter(
            Condition::all()
                .add(CBuild::Evaluation.eq(evaluation.id))
                .add(CBuild::Status.eq(BuildStatus::Failed)),
        )
        .count(&state.db)
        .await? as i64;

    let ep_builds: Vec<Uuid> = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation.id))
        .all(&state.db)
        .await?
        .into_iter()
        .map(|ep| ep.build)
        .collect();

    let (completed_entry_points, failed_entry_points, total_entry_points) = if ep_builds.is_empty()
    {
        (0i64, 0i64, 0i64)
    } else {
        let completed = EBuild::find()
            .filter(CBuild::Id.is_in(ep_builds.clone()))
            .filter(
                Condition::any()
                    .add(CBuild::Status.eq(BuildStatus::Completed))
                    .add(CBuild::Status.eq(BuildStatus::Substituted)),
            )
            .count(&state.db)
            .await? as i64;
        let failed = EBuild::find()
            .filter(CBuild::Id.is_in(ep_builds.clone()))
            .filter(CBuild::Status.eq(BuildStatus::Failed))
            .count(&state.db)
            .await? as i64;
        (completed, failed, ep_builds.len() as i64)
    };

    let entry_point_diff = if let Some(prev_id) = evaluation.previous {
        let prev_count = EEntryPoint::find()
            .filter(CEntryPoint::Evaluation.eq(prev_id))
            .count(&state.db)
            .await? as i64;
        Some(total_entry_points - prev_count)
    } else {
        None
    };

    Ok(EvaluationSummary {
        id: evaluation.id,
        commit: commit_hash,
        status: evaluation.status,
        total_builds,
        failed_builds,
        completed_entry_points,
        failed_entry_points,
        entry_point_diff,
        created_at: evaluation.created_at,
        updated_at: evaluation.updated_at,
    })
}

pub async fn post_project_evaluate(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_organization, project): (MOrganization, MProject) = get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await?
    .ok_or_else(|| WebError::not_found("Project"))?;

    let mut project_for_check = project.clone();
    project_for_check.force_evaluation = true;
    let (_has_updates, commit_hash) = check_project_updates(Arc::clone(&state), &project_for_check)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    if commit_hash.is_empty() {
        return Err(WebError::InternalServerError(
            "Failed to fetch repository state".to_string(),
        ));
    }

    core::ci::trigger_evaluation(&state.db, &project, commit_hash, None, None)
        .await
        .map_err(|e| match e {
            core::ci::TriggerError::AlreadyInProgress => {
                WebError::BadRequest("Evaluation already in progress".to_string())
            }
            core::ci::TriggerError::Db(db_err) => WebError::from(db_err),
        })?;

    let res = BaseResponse {
        error: false,
        message: "Evaluation started".to_string(),
    };

    Ok(Json(res))
}

/// `GET /projects/{organization}/{project}/evaluations`
///
/// Returns the `keep_evaluations` most recent evaluations for the project,
/// newest first. Identical access rules as other project endpoints.
pub async fn get_project_evaluations(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<Vec<EvaluationSummary>>>> {
    let organization = get_any_organization_by_name(state.0.clone(), organization)
        .await?
        .ok_or_else(|| WebError::not_found("Project"))?;

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
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(project.keep_evaluations as u64)
        .all(&state.db)
        .await?;

    let mut summaries = Vec::with_capacity(evaluations.len());
    for evaluation in evaluations {
        summaries.push(evaluation_to_summary(&state.0, evaluation).await?);
    }

    Ok(Json(BaseResponse {
        error: false,
        message: summaries,
    }))
}

pub async fn get_project_details(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<ProjectDetailsResponse>>> {
    let organization: MOrganization =
        get_any_organization_by_name(state.0.clone(), organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Project"))?;

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

    let project: MProject = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(project))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Project"))?;

    // Get last 5 evaluations for this project
    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(5)
        .all(&state.db)
        .await?;

    let mut evaluation_summaries = Vec::with_capacity(evaluations.len());
    for evaluation in evaluations {
        evaluation_summaries.push(evaluation_to_summary(&state.0, evaluation).await?);
    }

    let can_edit = match &maybe_user {
        Some(user) => user_can_edit(&state, user.id, organization.id).await?,
        None => false,
    };

    let project_details = ProjectDetailsResponse {
        id: project.id,
        name: project.name,
        display_name: project.display_name,
        description: project.description,
        repository: project.repository,
        evaluation_wildcard: project.evaluation_wildcard,
        active: project.active,
        created_at: project.created_at,
        keep_evaluations: project.keep_evaluations,
        last_evaluations: evaluation_summaries,
        can_edit,
    };

    let res = BaseResponse {
        error: false,
        message: project_details,
    };

    Ok(Json(res))
}

#[derive(Deserialize, Debug)]
pub struct EntryPointsQuery {
    pub evaluation_id: Option<Uuid>,
}

pub async fn get_project_entry_points(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
    Query(params): Query<EntryPointsQuery>,
) -> WebResult<Json<BaseResponse<Vec<EntryPointSummary>>>> {
    let organization: MOrganization =
        get_any_organization_by_name(state.0.clone(), organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Project"))?;

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

    let project: MProject = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(project))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Project"))?;

    // Use the requested evaluation ID, or fall back to the project's last evaluation.
    let eval_id = match params.evaluation_id.or(project.last_evaluation) {
        Some(id) => id,
        None => {
            return Ok(Json(BaseResponse {
                error: false,
                message: vec![],
            }));
        }
    };

    let evaluation = EEvaluation::find_by_id(eval_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    if evaluation.project != Some(project.id) {
        return Err(WebError::not_found("Evaluation"));
    }

    let entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(eval_id))
        .all(&state.db)
        .await?;

    if entry_points.is_empty() {
        return Ok(Json(BaseResponse {
            error: false,
            message: vec![],
        }));
    }

    let build_ids: Vec<Uuid> = entry_points.iter().map(|ep| ep.build).collect();
    let builds = EBuild::find()
        .filter(CBuild::Id.is_in(build_ids))
        .all(&state.db)
        .await?;
    let build_map: HashMap<Uuid, MBuild> = builds.into_iter().map(|b| (b.id, b)).collect();

    let drv_ids: Vec<Uuid> = build_map.values().map(|b| b.derivation).collect();
    let derivations: HashMap<Uuid, MDerivation> = if drv_ids.is_empty() {
        HashMap::new()
    } else {
        EDerivation::find()
            .filter(CDerivation::Id.is_in(drv_ids.clone()))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|d| (d.id, d))
            .collect()
    };

    let completed_drv_ids: Vec<Uuid> = entry_points
        .iter()
        .filter_map(|ep| build_map.get(&ep.build))
        .filter(|b| b.status == BuildStatus::Completed || b.status == BuildStatus::Substituted)
        .map(|b| b.derivation)
        .collect();

    let has_artefacts_map: HashMap<Uuid, bool> = if completed_drv_ids.is_empty() {
        HashMap::new()
    } else {
        let mut m: HashMap<Uuid, bool> = HashMap::new();
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(completed_drv_ids))
            .filter(CDerivationOutput::HasArtefacts.eq(true))
            .all(&state.db)
            .await?;
        for o in outputs {
            m.insert(o.derivation, true);
        }
        m
    };

    let mut summaries = Vec::new();
    for ep in entry_points {
        let build = match build_map.get(&ep.build) {
            Some(b) => b,
            None => continue,
        };
        let drv = match derivations.get(&build.derivation) {
            Some(d) => d,
            None => continue,
        };
        summaries.push(EntryPointSummary {
            id: ep.id,
            build_id: build.id,
            derivation_path: drv.derivation_path.clone(),
            eval: ep.eval.clone(),
            build_status: build.status.clone(),
            has_artefacts: *has_artefacts_map.get(&build.derivation).unwrap_or(&false),
            architecture: drv.architecture.clone(),
            evaluation_id: evaluation.id,
            evaluation_status: evaluation.status.clone(),
            created_at: ep.created_at,
        });
    }

    Ok(Json(BaseResponse {
        error: false,
        message: summaries,
    }))
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

/// Downloads the newest build output for a specific entry point.
///
/// Resolves the most recently completed evaluation for the project, finds the entry
/// point matching `eval`, and serves the named file from `nix-support/hydra-build-products`.
///
/// Authentication:
/// - Public organisations: no credentials required.
/// - Private organisations: supply `?token=GRADxxxx` (API key) or a JWT, **or** authenticate
///   via the `Authorization: Bearer` header / `jwt_token` session cookie.
pub async fn get_entry_point_download(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
    Query(params): Query<EntryPointDownloadQuery>,
) -> Result<Response, WebError> {
    let organization = get_any_organization_by_name(state.0.clone(), organization)
        .await?
        .ok_or_else(|| WebError::not_found("Organization"))?;

    let project = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(&project))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Project"))?;

    // Resolve caller identity from ?token= (API key / JWT) or existing session.
    let resolved_user: Option<MUser> = if let Some(token_str) = params.token {
        let token_data = crate::authorization::decode_jwt(State(Arc::clone(&state)), token_str)
            .await
            .map_err(|_| WebError::Unauthorized("Invalid token".to_string()))?;
        EUser::find_by_id(token_data.claims.id)
            .one(&state.db)
            .await?
    } else {
        maybe_user
    };

    if !organization.public {
        match resolved_user {
            Some(ref user) => {
                if !user_is_org_member(&state, user.id, organization.id).await? {
                    return Err(WebError::not_found("Project"));
                }
            }
            None => return Err(WebError::Unauthorized("Authorization required".to_string())),
        }
    }

    // Most recent completed evaluation for this project.
    let evaluation = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(CEvaluation::Status.eq(EvaluationStatus::Completed))
        .order_by_desc(CEvaluation::CreatedAt)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    // Entry point whose `eval` attribute path matches the query param.
    // Axum URL-decodes the value automatically, so %22 → " before this comparison.
    let ep = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation.id))
        .filter(CEntryPoint::Eval.eq(&params.eval))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Entry point"))?;

    let build = EBuild::find_by_id(ep.build)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    if build.status != BuildStatus::Completed && build.status != BuildStatus::Substituted {
        return Err(WebError::not_found("File"));
    }

    // Walk derivation outputs, locate the file via hydra-build-products.
    let build_outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(build.derivation))
        .all(&state.db)
        .await?;

    for output in &build_outputs {
        // Substituted builds may not have their output paths realised on the
        // gradient-server's local store yet. Ensure the path exists before
        // reading hydra-build-products.
        if let Err(e) = state.web_nix_store.ensure_path(output.output.clone()).await {
            tracing::warn!(
                error = format!("{:#}", e),
                path = %output.output,
                "Failed to ensure output path is realised"
            );
        }
    }

    for output in build_outputs {
        let hydra_products_path = format!("{}/nix-support/hydra-build-products", output.output);
        if let Ok(content) = fs::read_to_string(&hydra_products_path).await {
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 && parts[0] == "file" {
                    let file_path = parts[2..].join(" ");
                    let file_name = std::path::Path::new(&file_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    if file_name == params.filename {
                        match fs::read(&file_path).await {
                            Ok(contents) => {
                                let content_type = match std::path::Path::new(&params.filename)
                                    .extension()
                                    .and_then(|ext| ext.to_str())
                                {
                                    Some("tar") => "application/x-tar",
                                    Some("gz") => "application/gzip",
                                    Some("zst") => "application/zstd",
                                    Some("txt") => "text/plain",
                                    Some("json") => "application/json",
                                    Some("zip") => "application/zip",
                                    _ => "application/octet-stream",
                                };
                                return Ok((
                                    StatusCode::OK,
                                    [
                                        (header::CONTENT_TYPE, content_type),
                                        (
                                            header::CONTENT_DISPOSITION,
                                            &format!(
                                                "attachment; filename=\"{}\"",
                                                params.filename
                                            ),
                                        ),
                                    ],
                                    contents,
                                )
                                    .into_response());
                            }
                            Err(_) => {
                                return Err(WebError::InternalServerError(
                                    "Failed to read file".to_string(),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    Err(WebError::not_found("File"))
}
