use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use uuid::Uuid;

use super::types::*;
use super::requests::*;
use super::tables::*;


pub async fn get_organizations() -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organizations: ListResponse = vec![];

    let res = BaseResponse {
        error: false,
        message: organizations,
    };

    Ok(Json(res))
}

pub async fn post_organizations() -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "Organization added successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_organization(Path(organization_id) : Path<Uuid>) -> Result<Json<BaseResponse<Organization>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization = Organization {
        id: Uuid::nil(),
        name: "Organization Title".to_string(),
        description: "Organization Description".to_string(),
        created_by: Uuid::nil(),
        created_at: 0,
    };

    let res = BaseResponse {
        error: false,
        message: organization,
    };

    Ok(Json(res))
}

pub async fn post_organization(Path(organization_id) : Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "Project added successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_project(Path(project_id) : Path<Uuid>) -> Result<Json<BaseResponse<Project>>, (StatusCode, Json<BaseResponse<String>>)> {
    let project = Project {
        id: Uuid::nil(),
        organization_id: Uuid::nil(),
        name: "Project Title".to_string(),
        description: "Project Description".to_string(),
        last_check_at: 0,
        created_by: Uuid::nil(),
        created_at: 0,
    };

    let res = BaseResponse {
        error: false,
        message: project,
    };

    Ok(Json(res))
}

pub async fn post_project(Path(project_id) : Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "Project configured successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_build(Path(build_id) : Path<Uuid>) -> Result<Json<BaseResponse<Build>>, (StatusCode, Json<BaseResponse<String>>)> {
    let build = Build {
        id: Uuid::nil(),
        project_id: Uuid::nil(),
        path: "".to_string(),
        dependencies: vec![],
        created_at: 0,
    };

    let res = BaseResponse {
        error: false,
        message: build,
    };

    Ok(Json(res))
}

pub async fn post_build(Path(build_id) : Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "Build executed successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_servers() -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let servers: ListResponse = vec![];

    let res = BaseResponse {
        error: false,
        message: servers,
    };

    Ok(Json(res))
}

pub async fn post_servers() -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "Server added successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_user(Path(user_id) : Path<Uuid>) -> Result<Json<BaseResponse<User>>, (StatusCode, Json<BaseResponse<String>>)> {
    let user = User {
        id: Uuid::nil(),
        username: "username".to_string(),
        email: "email".to_string(),
        password_salt: "salt".to_string(),
        password: "password".to_string(),
        created_at: 0,
    };

    let res = BaseResponse {
        error: false,
        message: user,
    };

    Ok(Json(res))
}

pub async fn post_user(Path(user_id) : Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "User added successfully".to_string(),
    };

    Err((StatusCode::NOT_IMPLEMENTED, Json(BaseResponse {
        error: true,
        message: "not implemented yet".to_string(),
    })))
}

