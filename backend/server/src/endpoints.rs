use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use uuid::Uuid;
use sea_orm::{EntityTrait, ActiveModelTrait};
use sea_orm::ActiveValue::Set;
use chrono::Utc;
use std::sync::Arc;

use super::consts::*;
use super::types::*;
use super::requests::*;


// TODO: USER AUTHENTICATION + User specific endpoints
// TODO: sanitize inputs
pub async fn get_organizations(state: State<Arc<ServerState>>) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organizations = EOrganization::find().all(&state.db).await.unwrap();
    let organizations: ListResponse = organizations.iter().map(|o| (o.id.clone(), o.name.clone())).collect();

    let res = BaseResponse {
        error: false,
        message: organizations,
    };

    Ok(Json(res))
}

pub async fn post_organizations(state: State<Arc<ServerState>>, Json(body): Json<MakeOrganizationRequest>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization = AOrganization {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        description: Set(body.description.clone()),
        created_by: Set(Uuid::nil()),
        created_at: Set(Utc::now().naive_utc()),
    };

    let organization = organization.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: organization.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_organization(state: State<Arc<ServerState>>, Path(organization_id): Path<Uuid>) -> Result<Json<BaseResponse<MOrganization>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization = EOrganization::find_by_id(organization_id).one(&state.db).await.unwrap().unwrap();

    let res = BaseResponse {
        error: false,
        message: organization,
    };

    Ok(Json(res))
}

pub async fn post_organization(state: State<Arc<ServerState>>, Path(organization_id): Path<Uuid>, Json(body): Json<MakeProjectRequest>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let project = AProject {
        id: Set(Uuid::new_v4()),
        organization: Set(organization_id),
        name: Set(body.name.clone()),
        description: Set(body.description.clone()),
        repository: Set(body.repository.clone()),
        last_evaluation: Set(None),
        last_check_at: Set(*NULL_TIME),
        created_by: Set(Uuid::nil()),
        created_at: Set(Utc::now().naive_utc()),
    };

    let project = project.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: project.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_project(state: State<Arc<ServerState>>, Path(project_id): Path<Uuid>) -> Result<Json<BaseResponse<MProject>>, (StatusCode, Json<BaseResponse<String>>)> {
    let project = EProject::find_by_id(project_id).one(&state.db).await.unwrap().unwrap();

    let res = BaseResponse {
        error: false,
        message: project,
    };

    Ok(Json(res))
}

pub async fn post_project(state: State<Arc<ServerState>>, Path(project_id): Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "Project configured successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_build(state: State<Arc<ServerState>>, Path(build_id): Path<Uuid>) -> Result<Json<BaseResponse<MBuild>>, (StatusCode, Json<BaseResponse<String>>)> {
    let build = EBuild::find_by_id(build_id).one(&state.db).await.unwrap().unwrap();

    let res = BaseResponse {
        error: false,
        message: build,
    };

    Ok(Json(res))
}

pub async fn post_build(state: State<Arc<ServerState>>, Path(build_id): Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "Build executed successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_servers(state: State<Arc<ServerState>>) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let servers = EServer::find().all(&state.db).await.unwrap();
    let servers: ListResponse = servers.iter().map(|s| (s.id.clone(), s.name.clone())).collect();

    let res = BaseResponse {
        error: false,
        message: servers,
    };

    Ok(Json(res))
}

pub async fn post_servers(state: State<Arc<ServerState>>, Path(organization_id): Path<Uuid>, Json(body): Json<MakeServerRequest>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let server = AServer {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        organization: Set(organization_id),
        host: Set(body.host.clone()),
        port: Set(body.port),
        architectures: Set(body.architectures.clone()),
        features: Set(body.features.clone()),
        last_connection_at: Set(*NULL_TIME),
        created_by: Set(Uuid::nil()),
        created_at: Set(Utc::now().naive_utc()),
    };

    let server = server.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: server.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_user(state: State<Arc<ServerState>>, Path(user_id): Path<Uuid>) -> Result<Json<BaseResponse<MUser>>, (StatusCode, Json<BaseResponse<String>>)> {
    let user = EUser::find_by_id(user_id).one(&state.db).await.unwrap().unwrap();

    let res = BaseResponse {
        error: false,
        message: user,
    };

    Ok(Json(res))
}

pub async fn post_user(state: State<Arc<ServerState>>, Path(user_id): Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    Err((StatusCode::NOT_IMPLEMENTED, Json(BaseResponse {
        error: true,
        message: "not implemented yet".to_string(),
    })))
}

