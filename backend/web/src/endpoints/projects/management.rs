/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{user_can_edit, ProjectResponse};
use crate::authorization::MaybeUser;
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use chrono::Utc;
use core::ci::encrypt_webhook_secret;
use core::db::{get_any_organization_by_name, get_organization_by_name, get_project_by_name};
use core::nix::RepositoryUrl;
use core::sources::check_project_updates;
use core::types::consts::*;
use core::types::input::{check_index_name, validate_display_name, vec_to_hex};
use core::types::wildcard::Wildcard;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

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
    pub ci_reporter_type: Option<String>,
    pub ci_reporter_url: Option<String>,
    pub ci_reporter_token: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TransferOwnershipRequest {
    pub organization: String,
}

pub async fn get_project_name_available(
    state: State<Arc<ServerState>>,
    Path(organization): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_index_name(&name).is_err() {
        return Ok(Json(BaseResponse {
            error: false,
            message: false,
        }));
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
    let eval_status_map: HashMap<Uuid, entity::evaluation::EvaluationStatus> =
        if eval_ids.is_empty() {
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
            let last_evaluation_status = p
                .last_evaluation
                .and_then(|id| eval_status_map.get(&id).cloned());
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
                ci_reporter_type: p.ci_reporter_type,
                ci_reporter_url: p.ci_reporter_url,
            }
        })
        .collect();

    Ok(Json(BaseResponse {
        error: false,
        message: Paginated {
            items,
            total,
            page,
            per_page,
        },
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

    body.repository
        .parse::<RepositoryUrl>()
        .map_err(|e| WebError::BadRequest(e.to_string()))?;

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

    let evaluation_wildcard = body.evaluation_wildcard.trim()
        .parse::<Wildcard>()
        .map_err(|e| WebError::BadRequest(e.to_string()))?
        .to_string();

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
        ci_reporter_type: Set(None),
        ci_reporter_url: Set(None),
        ci_reporter_token: Set(None),
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
            ci_reporter_type: project.ci_reporter_type,
            ci_reporter_url: project.ci_reporter_url,
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
        repository
            .parse::<RepositoryUrl>()
            .map_err(|e| WebError::BadRequest(e.to_string()))?;
        aproject.repository = Set(repository);
    }

    if let Some(evaluation_wildcard) = body.evaluation_wildcard {
        let evaluation_wildcard = evaluation_wildcard.trim()
            .parse::<Wildcard>()
            .map_err(|e| WebError::BadRequest(e.to_string()))?
            .to_string();
        aproject.evaluation_wildcard = Set(evaluation_wildcard);
    }

    if let Some(keep) = body.keep_evaluations {
        if keep < 1 {
            return Err(WebError::BadRequest(
                "keep_evaluations must be at least 1".to_string(),
            ));
        }
        let global_max = state.cli.keep_evaluations as i32;
        if global_max > 0 && keep > global_max {
            return Err(WebError::BadRequest(format!(
                "keep_evaluations cannot exceed the server maximum of {}",
                global_max
            )));
        }
        aproject.keep_evaluations = Set(keep);
    }

    // CI reporter — treat empty string as "remove" (set to None)
    if let Some(ci_type) = body.ci_reporter_type {
        aproject.ci_reporter_type = Set(if ci_type.is_empty() { None } else { Some(ci_type) });
    }
    if let Some(ci_url) = body.ci_reporter_url {
        aproject.ci_reporter_url = Set(if ci_url.is_empty() { None } else { Some(ci_url) });
    }
    if let Some(ci_token) = body.ci_reporter_token {
        if ci_token.is_empty() {
            aproject.ci_reporter_token = Set(None);
        } else {
            let encrypted = encrypt_webhook_secret(&state.cli.crypt_secret_file, &ci_token)
                .map_err(|e| WebError::BadRequest(format!("Failed to encrypt CI token: {}", e)))?;
            aproject.ci_reporter_token = Set(Some(encrypted));
        }
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

pub async fn delete_project_integration(
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
            "You do not have permission to modify this project.".to_string(),
        ));
    }

    if project.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed project.".to_string(),
        ));
    }

    let mut aproject: AProject = project.into();
    aproject.ci_reporter_type = Set(None);
    aproject.ci_reporter_url = Set(None);
    aproject.ci_reporter_token = Set(None);
    aproject.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Integration removed".to_string(),
    }))
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
