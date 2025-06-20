/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::{Extension, Json};
use crate::error::{WebError, WebResult};
use chrono::Utc;
use core::consts::*;
use core::database::{get_organization_by_name, get_project_by_name};
use core::input::{check_index_name, valid_evaluation_wildcard, vec_to_hex};
use core::sources::check_project_updates;
use core::types::*;
use entity::evaluation::EvaluationStatus;
use git_url_parse::normalize_url;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
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
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<ListResponse>>> {
    // TODO: Implement pagination
    let organization: MOrganization = get_organization_by_name(state.0.clone(), user.id, organization.clone()).await
        .ok_or_else(|| WebError::not_found("Organization"))?;

    let projects = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .all(&state.db)
        .await?;

    let projects: ListResponse = projects
        .iter()
        .map(|p| ListItem {
            id: p.id,
            name: p.name.clone(),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: projects,
    };

    Ok(Json(res))
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

    let repository_url = normalize_url(body.repository.clone().as_str())
        .map_err(|_| WebError::BadRequest("Invalid Repository URL".to_string()))?;

    let organization: MOrganization = get_organization_by_name(state.0.clone(), user.id, organization.clone()).await
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

    if !valid_evaluation_wildcard(body.evaluation_wildcard.clone().as_str()) {
        return Err(WebError::BadRequest("Invalid Evaluation Wildcard".to_string()));
    }

    let project = AProject {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        name: Set(body.name.clone()),
        active: Set(true),
        display_name: Set(body.display_name.clone()),
        description: Set(body.description.clone()),
        repository: Set(repository_url.to_string()),
        evaluation_wildcard: Set(body.evaluation_wildcard.clone()),
        last_evaluation: Set(None),
        last_check_at: Set(*NULL_TIME),
        force_evaluation: Set(false),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
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
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<MProject>>> {
    let (_organization, project): (MOrganization, MProject) = get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await
    .ok_or_else(|| WebError::not_found("Project"))?;

    let res = BaseResponse {
        error: false,
        message: project,
    };

    Ok(Json(res))
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
    .await
    .ok_or_else(|| WebError::not_found("Project"))?;

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
        aproject.display_name = Set(display_name);
    }

    if let Some(description) = body.description {
        aproject.description = Set(description);
    }

    if let Some(repository) = body.repository {
        let repository_url = normalize_url(repository.as_str())
            .map_err(|_| WebError::BadRequest("Invalid Repository URL".to_string()))?;

        aproject.repository = Set(repository_url.to_string());
    }

    if let Some(evaluation_wildcard) = body.evaluation_wildcard {
        if !valid_evaluation_wildcard(evaluation_wildcard.as_str()) {
            return Err(WebError::BadRequest("Invalid Evaluation Wildcard".to_string()));
        }

        aproject.evaluation_wildcard = Set(evaluation_wildcard);
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
    let (_organization, project): (MOrganization, MProject) = get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await
    .ok_or_else(|| WebError::not_found("Project"))?;

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
    .await
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
    .await
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
    .await
    .ok_or_else(|| WebError::not_found("Project"))?;

    let (_has_updates, remote_hash) = check_project_updates(Arc::clone(&state), &project).await;

    if !remote_hash.is_empty() {
        let res = BaseResponse {
            error: false,
            message: vec_to_hex(&remote_hash),
        };

        Ok(Json(res))
    } else {
        Err(WebError::InternalServerError("Failed to check repository".to_string()))
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
    .await
    .ok_or_else(|| WebError::not_found("Project"))?;

    if let Some(evaluation_id) = project.last_evaluation {
        let evaluation: MEvaluation = EEvaluation::find_by_id(evaluation_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("Evaluation {} not found for project {}", evaluation_id, project.id);
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;

        if evaluation.status == EvaluationStatus::Queued
            || evaluation.status == EvaluationStatus::Evaluating
            || evaluation.status == EvaluationStatus::Building
        {
            return Err(WebError::BadRequest("Evaluation already in progress".to_string()));
        }
    }

    let mut aproject: AProject = project.into();

    aproject.last_check_at = Set(*NULL_TIME);
    aproject.force_evaluation = Set(true);
    aproject.save(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "Evaluation started".to_string(),
    };

    Ok(Json(res))
}
