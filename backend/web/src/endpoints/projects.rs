/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
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
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Implement pagination
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let projects = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .all(&state.db)
        .await
        .unwrap();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Project Name".to_string(),
            }),
        ));
    }

    let repository_url = normalize_url(body.repository.clone().as_str()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Repository URL".to_string(),
            }),
        )
    })?;

    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let project = EProject::find()
        .filter(
            Condition::all()
                .add(CProject::Organization.eq(organization.id))
                .add(CProject::Name.eq(body.name.clone())),
        )
        .one(&state.db)
        .await
        .unwrap();

    if project.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "Project Name already exists".to_string(),
            }),
        ));
    };

    if !valid_evaluation_wildcard(body.evaluation_wildcard.clone().as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Evaluation Wildcard".to_string(),
            }),
        ));
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

    let project = project.insert(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<MProject>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (_organization, project): (MOrganization, MProject) = match get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await
    {
        Some((o, p)) => (o, p),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Project not found".to_string(),
                }),
            ))
        }
    };

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (organization, project): (MOrganization, MProject) = match get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await
    {
        Some((o, p)) => (o, p),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Project not found".to_string(),
                }),
            ))
        }
    };

    let mut aproject: AProject = project.into();

    if let Some(name) = body.name {
        if check_index_name(name.as_str()).is_err() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid Project Name".to_string(),
                }),
            ));
        }

        let project = EProject::find()
            .filter(
                Condition::all()
                    .add(CProject::Organization.eq(organization.id))
                    .add(CProject::Name.eq(name.clone())),
            )
            .one(&state.db)
            .await
            .unwrap();

        if project.is_some() {
            return Err((
                StatusCode::CONFLICT,
                Json(BaseResponse {
                    error: true,
                    message: "Project Name already exists".to_string(),
                }),
            ));
        };

        aproject.name = Set(name);
    }

    if let Some(display_name) = body.display_name {
        aproject.display_name = Set(display_name);
    }

    if let Some(description) = body.description {
        aproject.description = Set(description);
    }

    if let Some(repository) = body.repository {
        let repository_url = normalize_url(repository.as_str()).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid Repository URL".to_string(),
                }),
            )
        })?;

        aproject.repository = Set(repository_url.to_string());
    }

    if let Some(evaluation_wildcard) = body.evaluation_wildcard {
        if !valid_evaluation_wildcard(evaluation_wildcard.as_str()) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid Evaluation Wildcard".to_string(),
                }),
            ));
        }
    }

    aproject.update(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (_organization, project): (MOrganization, MProject) = match get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await
    {
        Some((o, p)) => (o, p),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Project not found".to_string(),
                }),
            ))
        }
    };

    let aproject: AProject = project.into();
    aproject.delete(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (_organization, project): (MOrganization, MProject) = match get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await
    {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Project not found".to_string(),
                }),
            ))
        }
    };

    let mut aproject: AProject = project.into();
    aproject.active = Set(true);
    aproject.update(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (_organization, project): (MOrganization, MProject) = match get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await
    {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Project not found".to_string(),
                }),
            ))
        }
    };

    let mut aproject: AProject = project.into();
    aproject.active = Set(false);
    aproject.update(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (_organization, project): (MOrganization, MProject) = match get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await
    {
        Some((o, p)) => (o, p),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Project not found".to_string(),
                }),
            ))
        }
    };

    let (_has_updates, remote_hash) = check_project_updates(Arc::clone(&state), &project).await;

    if !remote_hash.is_empty() {
        let res = BaseResponse {
            error: false,
            message: vec_to_hex(&remote_hash),
        };

        Ok(Json(res))
    } else {
        Err((
            StatusCode::GATEWAY_TIMEOUT,
            Json(BaseResponse {
                error: true,
                message: "Failed to check repository".to_string(),
            }),
        ))
    }
}

pub async fn post_project_evaluate(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (_organization, project): (MOrganization, MProject) = match get_project_by_name(
        state.0.clone(),
        user.id,
        organization.clone(),
        project.clone(),
    )
    .await
    {
        Some((o, p)) => (o, p),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Project not found".to_string(),
                }),
            ))
        }
    };

    if let Some(evaluation_id) = project.last_evaluation {
        let evaluation: MEvaluation = EEvaluation::find_by_id(evaluation_id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();

        if evaluation.status == EvaluationStatus::Queued
            || evaluation.status == EvaluationStatus::Evaluating
            || evaluation.status == EvaluationStatus::Building
        {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "Evaluation already in progress".to_string(),
                }),
            ));
        }
    }

    let mut aproject: AProject = project.into();

    aproject.last_check_at = Set(*NULL_TIME);
    aproject.force_evaluation = Set(true);
    aproject.save(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: "Evaluation started".to_string(),
    };

    Ok(Json(res))
}
