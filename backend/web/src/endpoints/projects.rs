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
use chrono::Utc;
use core::consts::*;
use core::database::{
    get_any_organization_by_name, get_organization_by_name, get_project_by_name,
};
use core::input::{check_index_name, valid_evaluation_wildcard, validate_display_name, vec_to_hex};
use core::sources::check_project_updates;
use core::types::*;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use git_url_parse::GitUrl;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectResponse {
    pub id: Uuid,
    pub organization: Uuid,
    pub name: String,
    pub active: bool,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
    pub last_evaluation: Option<Uuid>,
    pub last_evaluation_status: Option<EvaluationStatus>,
    pub force_evaluation: bool,
    pub created_by: Uuid,
    pub created_at: chrono::NaiveDateTime,
    pub managed: bool,
    pub keep_evaluations: i32,
    pub can_edit: bool,
}

/// Returns true if the user has Admin or Write role in the organization.
pub(crate) async fn user_can_edit(
    state: &Arc<ServerState>,
    user_id: Uuid,
    organization_id: Uuid,
) -> Result<bool, WebError> {
    let org_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization_id))
                .add(COrganizationUser::User.eq(user_id)),
        )
        .one(&state.db)
        .await?;

    Ok(match org_user {
        Some(ou) => ou.role == BASE_ROLE_ADMIN_ID || ou.role == BASE_ROLE_WRITE_ID,
        None => false,
    })
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeProjectRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchProjectRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub evaluation_wildcard: Option<String>,
    pub keep_evaluations: Option<i32>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TransferOwnershipRequest {
    pub organization: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointSummary {
    pub id: Uuid,
    pub build_id: Uuid,
    pub derivation_path: String,
    pub build_status: BuildStatus,
    pub has_artefacts: bool,
    pub architecture: entity::server::Architecture,
    pub evaluation_id: Uuid,
    pub evaluation_status: EvaluationStatus,
    pub created_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EvaluationSummary {
    pub id: Uuid,
    pub commit: String,
    pub status: EvaluationStatus,
    pub total_builds: i64,
    pub failed_builds: i64,
    pub completed_entry_points: i64,
    pub failed_entry_points: i64,
    pub entry_point_diff: Option<i64>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectDetailsResponse {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
    pub active: bool,
    pub created_at: chrono::NaiveDateTime,
    pub keep_evaluations: i32,
    pub last_evaluations: Vec<EvaluationSummary>,
    pub can_edit: bool,
}

pub async fn get_project_name_available(
    state: State<Arc<ServerState>>,
    Path(organization): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_index_name(&name).is_err() {
        return Ok(Json(BaseResponse { error: false, message: false }));
    }
    let org = get_any_organization_by_name(state.0.clone(), organization)
        .await?
        .ok_or_else(|| WebError::not_found("Organization"))?;
    let exists = EProject::find()
        .filter(CProject::Name.eq(name.as_str()))
        .filter(CProject::Organization.eq(org.id))
        .one(&state.db)
        .await?
        .is_some();
    Ok(Json(BaseResponse {
        error: false,
        message: !exists,
    }))
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(organization): Path<String>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<Vec<ProjectResponse>>>>> {
    let organization: MOrganization =
        get_any_organization_by_name(state.0.clone(), organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if !organization.public {
        match &maybe_user {
            Some(user) => {
                if !user_is_org_member(&state.0, user.id, organization.id).await? {
                    return Err(WebError::not_found("Organization"));
                }
            }
            None => return Err(WebError::not_found("Organization")),
        }
    }

    let page = params.page();
    let per_page = params.per_page();
    let can_edit = match &maybe_user {
        Some(user) => user_can_edit(&state, user.id, organization.id).await?,
        None => false,
    };

    let paginator = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .order_by_asc(CProject::CreatedAt)
        .paginate(&state.db, per_page);

    let total = paginator.num_items().await?;
    let raw = paginator.fetch_page(page - 1).await?;

    // Batch-fetch the status of the last evaluation for each project.
    let eval_ids: Vec<Uuid> = raw.iter().filter_map(|p| p.last_evaluation).collect();
    let eval_status_map: HashMap<Uuid, EvaluationStatus> = if eval_ids.is_empty() {
        HashMap::new()
    } else {
        EEvaluation::find()
            .filter(CEvaluation::Id.is_in(eval_ids))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|e| (e.id, e.status))
            .collect()
    };

    let items: Vec<ProjectResponse> = raw
        .into_iter()
        .map(|p| {
            let last_evaluation_status = p.last_evaluation.and_then(|id| eval_status_map.get(&id).cloned());
            ProjectResponse {
                id: p.id,
                organization: p.organization,
                name: p.name,
                active: p.active,
                display_name: p.display_name,
                description: p.description,
                repository: p.repository,
                evaluation_wildcard: p.evaluation_wildcard,
                last_evaluation: p.last_evaluation,
                last_evaluation_status,
                force_evaluation: p.force_evaluation,
                keep_evaluations: p.keep_evaluations,
                created_by: p.created_by,
                created_at: p.created_at,
                managed: p.managed,
                can_edit,
            }
        })
        .collect();

    Ok(Json(BaseResponse {
        error: false,
        message: Paginated { items, total, page, per_page },
    }))
}

pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<MakeProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err(WebError::invalid_name("Project Name"));
    }

    if let Err(e) = validate_display_name(&body.display_name) {
        return Err(WebError::BadRequest(format!("Invalid display name: {}", e)));
    }

    GitUrl::parse(&body.repository)
        .map_err(|_| WebError::BadRequest("Invalid Repository URL".to_string()))?;

    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let existing_project = EProject::find()
        .filter(
            Condition::all()
                .add(CProject::Organization.eq(organization.id))
                .add(CProject::Name.eq(body.name.clone())),
        )
        .one(&state.db)
        .await?;

    if existing_project.is_some() {
        return Err(WebError::already_exists("Project Name"));
    }

    let evaluation_wildcard = body.evaluation_wildcard.trim().to_string();
    if !valid_evaluation_wildcard(&evaluation_wildcard) {
        return Err(WebError::BadRequest(
            "Invalid Evaluation Wildcard".to_string(),
        ));
    }

    let project = AProject {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        name: Set(body.name.clone()),
        active: Set(true),
        display_name: Set(body.display_name.trim().to_string()),
        description: Set(body.description.trim().to_string()),
        repository: Set(body.repository.clone()),
        evaluation_wildcard: Set(evaluation_wildcard),
        last_evaluation: Set(None),
        last_check_at: Set(*NULL_TIME),
        force_evaluation: Set(false),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
        managed: Set(false),
        keep_evaluations: Set(30),
    };

    let project = project.insert(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: project.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_project(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<ProjectResponse>>> {
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

    let can_edit = match &maybe_user {
        Some(user) => user_can_edit(&state, user.id, organization.id).await?,
        None => false,
    };

    let last_evaluation_status = if let Some(eval_id) = project.last_evaluation {
        EEvaluation::find_by_id(eval_id)
            .one(&state.db)
            .await?
            .map(|e| e.status)
    } else {
        None
    };

    Ok(Json(BaseResponse {
        error: false,
        message: ProjectResponse {
            id: project.id,
            organization: project.organization,
            name: project.name,
            active: project.active,
            display_name: project.display_name,
            description: project.description,
            repository: project.repository,
            evaluation_wildcard: project.evaluation_wildcard,
            last_evaluation: project.last_evaluation,
            last_evaluation_status,
            force_evaluation: project.force_evaluation,
            created_by: project.created_by,
            created_at: project.created_at,
            managed: project.managed,
            keep_evaluations: project.keep_evaluations,
            can_edit,
        },
    }))
}

pub async fn patch_project(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
    Json(body): Json<PatchProjectRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (organization, project): (MOrganization, MProject) = get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await?
    .ok_or_else(|| WebError::not_found("Project"))?;

    if !user_can_edit(&state, user.id, organization.id).await? {
        return Err(WebError::Forbidden(
            "You do not have permission to modify this project.".to_string(),
        ));
    }

    // Prevent modification of state-managed projects
    if project.managed {
        return Err(WebError::Forbidden("Cannot modify state-managed project. This project is managed by configuration and cannot be edited through the API.".to_string()));
    }

    let mut aproject: AProject = project.into();

    if let Some(name) = body.name {
        if check_index_name(name.as_str()).is_err() {
            return Err(WebError::invalid_name("Project Name"));
        }

        let existing_project = EProject::find()
            .filter(
                Condition::all()
                    .add(CProject::Organization.eq(organization.id))
                    .add(CProject::Name.eq(name.clone())),
            )
            .one(&state.db)
            .await?;

        if existing_project.is_some() {
            return Err(WebError::already_exists("Project Name"));
        }

        aproject.name = Set(name);
    }

    if let Some(display_name) = body.display_name {
        let display_name = display_name.trim().to_string();
        if let Err(e) = validate_display_name(&display_name) {
            return Err(WebError::BadRequest(format!("Invalid display name: {}", e)));
        }
        aproject.display_name = Set(display_name);
    }

    if let Some(description) = body.description {
        aproject.description = Set(description.trim().to_string());
    }

    if let Some(repository) = body.repository {
        GitUrl::parse(&repository)
            .map_err(|_| WebError::BadRequest("Invalid Repository URL".to_string()))?;

        aproject.repository = Set(repository.clone());
    }

    if let Some(evaluation_wildcard) = body.evaluation_wildcard {
        let evaluation_wildcard = evaluation_wildcard.trim().to_string();
        if !valid_evaluation_wildcard(&evaluation_wildcard) {
            return Err(WebError::BadRequest(
                "Invalid Evaluation Wildcard".to_string(),
            ));
        }

        aproject.evaluation_wildcard = Set(evaluation_wildcard);
    }

    if let Some(keep) = body.keep_evaluations {
        if keep < 1 {
            return Err(WebError::BadRequest("keep_evaluations must be at least 1".to_string()));
        }
        aproject.keep_evaluations = Set(keep);
    }

    aproject.force_evaluation = Set(true);
    aproject.update(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "Project updated".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_project(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (organization, project): (MOrganization, MProject) = get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await?
    .ok_or_else(|| WebError::not_found("Project"))?;

    if !user_can_edit(&state, user.id, organization.id).await? {
        return Err(WebError::Forbidden(
            "You do not have permission to delete this project.".to_string(),
        ));
    }

    // Prevent deletion of state-managed projects
    if project.managed {
        return Err(WebError::Forbidden("Cannot delete state-managed project. This project is managed by configuration and cannot be deleted through the API.".to_string()));
    }

    let aproject: AProject = project.into();
    aproject.delete(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "Project deleted".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_project_active(
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

    let mut aproject: AProject = project.into();
    aproject.active = Set(true);
    aproject.update(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "Project enabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_project_active(
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

    let mut aproject: AProject = project.into();
    aproject.active = Set(false);
    aproject.update(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "Project disabled".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_project_check_repository(
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

    let (_has_updates, remote_hash) = check_project_updates(Arc::clone(&state), &project)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    if !remote_hash.is_empty() {
        let res = BaseResponse {
            error: false,
            message: vec_to_hex(&remote_hash),
        };

        Ok(Json(res))
    } else {
        Err(WebError::InternalServerError(
            "Failed to check repository".to_string(),
        ))
    }
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

    if let Some(evaluation_id) = project.last_evaluation {
        let evaluation: MEvaluation = EEvaluation::find_by_id(evaluation_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Evaluation {} not found for project {}",
                    evaluation_id,
                    project.id
                );
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;

        if evaluation.status == EvaluationStatus::Queued
            || evaluation.status == EvaluationStatus::Evaluating
            || evaluation.status == EvaluationStatus::Building
        {
            return Err(WebError::BadRequest(
                "Evaluation already in progress".to_string(),
            ));
        }
    }

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

    let now = Utc::now().naive_utc();

    let acommit = ACommit {
        id: Set(Uuid::new_v4()),
        message: Set(String::new()),
        hash: Set(commit_hash),
        author: Set(None),
        author_name: Set(String::new()),
    };
    let commit = acommit.insert(&state.db).await?;

    let aevaluation = AEvaluation {
        id: Set(Uuid::new_v4()),
        project: Set(Some(project.id)),
        repository: Set(project.repository.clone()),
        commit: Set(commit.id),
        wildcard: Set(project.evaluation_wildcard.clone()),
        status: Set(EvaluationStatus::Queued),
        previous: Set(project.last_evaluation),
        next: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        error: Set(None),
    };
    let evaluation = aevaluation.insert(&state.db).await?;

    let mut aproject: AProject = project.into();

    aproject.last_check_at = Set(*NULL_TIME);
    aproject.last_evaluation = Set(Some(evaluation.id));
    aproject.force_evaluation = Set(true);
    aproject.save(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "Evaluation started".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_project_transfer(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
    Json(body): Json<TransferOwnershipRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (organization, project): (MOrganization, MProject) = get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await?
    .ok_or_else(|| WebError::not_found("Project"))?;

    // Only admins of the org or the current owner may transfer ownership
    let is_admin = user_can_edit(&state, user.id, organization.id).await?;
    let is_owner = project.created_by == user.id;
    if !is_admin && !is_owner {
        return Err(WebError::Forbidden(
            "Only the project owner or an organization admin can transfer ownership.".to_string(),
        ));
    }

    if project.managed {
        return Err(WebError::Forbidden(
            "Cannot transfer ownership of a state-managed project.".to_string(),
        ));
    }

    let new_organization =
        get_organization_by_name(state.0.clone(), user.id, body.organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if new_organization.id == organization.id {
        return Err(WebError::BadRequest(
            "Project is already in this organization.".to_string(),
        ));
    }

    let mut aproject: AProject = project.into();
    aproject.organization = Set(new_organization.id);
    aproject.update(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "Ownership transferred".to_string(),
    };

    Ok(Json(res))
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

    let mut evaluation_summaries = Vec::new();

    for evaluation in evaluations {
        let commit_hash = ECommit::find_by_id(evaluation.commit)
            .one(&state.db)
            .await?
            .map(|c| vec_to_hex(&c.hash))
            .unwrap_or_default();

        // Count total builds for this evaluation
        let total_builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation.id))
            .count(&state.db)
            .await?;

        // Count failed builds for this evaluation
        let failed_builds = EBuild::find()
            .filter(
                Condition::all()
                    .add(CBuild::Evaluation.eq(evaluation.id))
                    .add(CBuild::Status.eq(BuildStatus::Failed)),
            )
            .count(&state.db)
            .await?;

        // Get entry point build IDs for this evaluation
        let ep_builds: Vec<Uuid> = EEntryPoint::find()
            .filter(CEntryPoint::Evaluation.eq(evaluation.id))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|ep| ep.build)
            .collect();

        let (completed_entry_points, failed_entry_points, total_entry_points) = if ep_builds.is_empty() {
            (0i64, 0i64, 0i64)
        } else {
            let completed = EBuild::find()
                .filter(CBuild::Id.is_in(ep_builds.clone()))
                .filter(CBuild::Status.eq(BuildStatus::Completed))
                .count(&state.db)
                .await? as i64;
            let failed = EBuild::find()
                .filter(CBuild::Id.is_in(ep_builds.clone()))
                .filter(CBuild::Status.eq(BuildStatus::Failed))
                .count(&state.db)
                .await? as i64;
            (completed, failed, ep_builds.len() as i64)
        };

        // Compute diff against previous evaluation's entry point count
        let entry_point_diff = if let Some(prev_id) = evaluation.previous {
            let prev_count = EEntryPoint::find()
                .filter(CEntryPoint::Evaluation.eq(prev_id))
                .count(&state.db)
                .await? as i64;
            Some(total_entry_points - prev_count)
        } else {
            None
        };

        evaluation_summaries.push(EvaluationSummary {
            id: evaluation.id,
            commit: commit_hash,
            status: evaluation.status,
            total_builds: total_builds as i64,
            failed_builds: failed_builds as i64,
            completed_entry_points,
            failed_entry_points,
            entry_point_diff,
            created_at: evaluation.created_at,
            updated_at: evaluation.updated_at,
        });
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

    let completed_ids: Vec<Uuid> = entry_points
        .iter()
        .filter_map(|ep| build_map.get(&ep.build))
        .filter(|b| b.status == BuildStatus::Completed)
        .map(|b| b.id)
        .collect();

    let has_artefacts_map: HashMap<Uuid, bool> = if completed_ids.is_empty() {
        HashMap::new()
    } else {
        EBuildOutput::find()
            .filter(CBuildOutput::Build.is_in(completed_ids))
            .filter(CBuildOutput::HasArtefacts.eq(true))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|o| (o.build, true))
            .collect()
    };

    let mut summaries = Vec::new();
    for ep in entry_points {
        let build = match build_map.get(&ep.build) {
            Some(b) => b,
            None => continue,
        };
        summaries.push(EntryPointSummary {
            id: ep.id,
            build_id: build.id,
            derivation_path: build.derivation_path.clone(),
            build_status: build.status.clone(),
            has_artefacts: *has_artefacts_map.get(&build.id).unwrap_or(&false),
            architecture: build.architecture.clone(),
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
        let eval_time_ms = (evaluation.updated_at - evaluation.created_at)
            .num_milliseconds();

        // Sum build durations for all completed builds
        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation.id))
            .all(&state.db)
            .await?;

        let build_time_total_ms: i64 = builds
            .iter()
            .filter(|b| b.status == BuildStatus::Completed)
            .map(|b| {
                b.build_time_ms
                    .unwrap_or_else(|| (b.updated_at - b.created_at).num_milliseconds())
            })
            .sum();

        let total_build_count = builds.len() as i64;

        // Entry points for this evaluation
        let ep_build_ids: Vec<Uuid> = EEntryPoint::find()
            .filter(CEntryPoint::Evaluation.eq(evaluation.id))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|ep| ep.build)
            .collect();

        let entry_point_count = ep_build_ids.len() as i64;
        let dependencies_count = total_build_count - entry_point_count;

        // Output size: file_size of entry-point build outputs only
        let output_size_bytes = if ep_build_ids.is_empty() {
            None
        } else {
            let outputs = EBuildOutput::find()
                .filter(CBuildOutput::Build.is_in(ep_build_ids))
                .all(&state.db)
                .await?;
            let total: i64 = outputs.iter().filter_map(|o| o.file_size).sum();
            if total > 0 { Some(total) } else { None }
        };

        // Closure size: file_size of ALL build outputs in this evaluation
        let all_build_ids: Vec<Uuid> = builds.iter().map(|b| b.id).collect();
        let closure_size_bytes = if all_build_ids.is_empty() {
            None
        } else {
            let outputs = EBuildOutput::find()
                .filter(CBuildOutput::Build.is_in(all_build_ids))
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
