use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use uuid::Uuid;

use super::types::*;
use super::requests::*;
use super::tables::*;


pub async fn get_projects() -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: get list of projects from database

    let projects: ListResponse = vec![];

    let res = BaseResponse {
        error: false,
        message: projects,
    };

    Ok(Json(res))
}

pub async fn post_projects() -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Add new project to database

    let res = BaseResponse {
        error: false,
        message: "Project added successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_project(Path(project_id) : Path<Uuid>) -> Result<Json<BaseResponse<Project>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Get list of jobsets for project from database

    let project = Project {
        id: Uuid::nil(),
        title: "Project Title".to_string(),
        description: "Project Description".to_string(),
    };

    let res = BaseResponse {
        error: false,
        message: project,
    };

    Ok(Json(res))
}

pub async fn post_project(Path(project_id) : Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Add new jobset to project or configure Project

    let res = BaseResponse {
        error: false,
        message: "Jobset added successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_jobset(Path(jobset_id) : Path<Uuid>) -> Result<Json<BaseResponse<Jobset>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Get jobset details

    let jobset = Jobset {
        id: Uuid::nil(),
        project_id: Uuid::nil(),
        title: "Jobset Title".to_string(),
        description: "Jobset Description".to_string(),
    };

    let res = BaseResponse {
        error: false,
        message: jobset,
    };

    Ok(Json(res))
}

pub async fn post_jobset(Path(jobset_id) : Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Configure or Execute jobset details

    let res = BaseResponse {
        error: false,
        message: "Jobset configured successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_build(Path(build_id) : Path<Uuid>) -> Result<Json<BaseResponse<Build>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Execute jobset

    let build = Build {
        id: Uuid::nil(),
        jobset_id: Uuid::nil(),
    };

    let res = BaseResponse {
        error: false,
        message: build,
    };

    Ok(Json(res))
}

pub async fn post_build(Path(build_id) : Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Execute jobset

    let res = BaseResponse {
        error: false,
        message: "Jobset executed successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_user(Path(user_id) : Path<Uuid>) -> Result<Json<BaseResponse<User>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Execute jobset

    let user = User {
        id: Uuid::nil(),
        username: "username".to_string(),
        email: "email".to_string(),
        password: "password".to_string(),
    };

    let res = BaseResponse {
        error: false,
        message: user,
    };

    Ok(Json(res))
}

pub async fn post_user(Path(user_id) : Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Execute jobset

    let res = BaseResponse {
        error: false,
        message: "User added successfully".to_string(),
    };

    Err((StatusCode::NOT_IMPLEMENTED, Json(BaseResponse {
        error: true,
        message: "not implemented yet".to_string(),
    })))
}

