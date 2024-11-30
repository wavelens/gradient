use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, Extension};
use uuid::Uuid;
use sea_orm::{EntityTrait, ActiveModelTrait, QueryFilter, QuerySelect, ColumnTrait, Condition, JoinType, RelationTrait};
use sea_orm::ActiveValue::Set;
use chrono::Utc;
use std::sync::Arc;
use password_auth::{verify_password, generate_hash};
use axum_streams::*;

use super::consts::*;
use super::types::*;
use super::requests::*;
use super::auth::{encode_jwt, generate_api_key, update_last_login};
use super::executer::get_buildlog_stream;


// TODO: USER AUTHENTICATION + User specific endpoints
// TODO: sanitize inputs
// TODO: use macros

pub async fn handle_404() -> (StatusCode, Json<BaseResponse<String>>) {
    (StatusCode::NOT_FOUND, Json(BaseResponse {
        error: true,
        message: "Not Found".to_string(),
    }))
}

pub async fn get_organizations(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organizations = EOrganization::find()
        .filter(COrganization::CreatedBy.eq(user.id))
        .all(&state.db)
        .await
        .unwrap();

    let organizations: ListResponse = organizations.iter().map(|o| (o.id, o.name.clone())).collect();

    let res = BaseResponse {
        error: false,
        message: organizations,
    };

    Ok(Json(res))
}

pub async fn get_health() -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "200 ALIVE".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_organizations(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Json(body): Json<MakeOrganizationRequest>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
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

pub async fn get_organization(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Path(organization_id): Path<Uuid>) -> Result<Json<BaseResponse<MOrganization>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization = match EOrganization::find_by_id(organization_id).one(&state.db).await.unwrap() {
        Some(o) => o,
        None => return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Organization not found".to_string(),
        }))),
    };

    if organization.created_by != user.id {
        return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Organization not found".to_string(),
        })));
    }

    let res = BaseResponse {
        error: false,
        message: organization,
    };

    Ok(Json(res))
}

pub async fn post_organization(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Path(organization_id): Path<Uuid>, Json(body): Json<MakeProjectRequest>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization = match EOrganization::find_by_id(organization_id).one(&state.db).await.unwrap() {
        Some(o) => o,
        None => return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Organization not found".to_string(),
        }))),
    };

    if organization.created_by != user.id {
        return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Organization not found".to_string(),
        })));
    }

    let project = EProject::find()
        .filter(Condition::all()
            .add(CProject::Organization.eq(organization.id))
            .add(CProject::Name.eq(body.name.clone()))
        )
        .one(&state.db).await.unwrap();

    if project.is_some() {
        return Err((StatusCode::CONFLICT, Json(BaseResponse {
            error: true,
            message: "Project Name already exists".to_string(),
        })));
    };


    let project = AProject {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        name: Set(body.name.clone()),
        description: Set(body.description.clone()),
        repository: Set(body.repository.clone()),
        last_evaluation: Set(None),
        last_check_at: Set(*NULL_TIME),
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

pub async fn get_project(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Path(project_id): Path<Uuid>) -> Result<Json<BaseResponse<MProject>>, (StatusCode, Json<BaseResponse<String>>)> {
    let project = match EProject::find_by_id(project_id).one(&state.db).await.unwrap() {
        Some(p) => p,
        None => return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Project not found".to_string(),
        }))),
    };

    let organization = EOrganization::find_by_id(project.organization).one(&state.db).await.unwrap().unwrap();

    if organization.created_by != user.id {
        return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Project not found".to_string(),
        })));
    }

    let res = BaseResponse {
        error: false,
        message: project,
    };

    Ok(Json(res))
}

pub async fn post_project(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Path(project_id): Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "Project configured successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_build(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Path(build_id): Path<Uuid>) -> Result<Json<BaseResponse<MBuild>>, (StatusCode, Json<BaseResponse<String>>)> {
    let build = match EBuild::find_by_id(build_id).one(&state.db).await.unwrap() {
        Some(b) => b,
        None => return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Build not found".to_string(),
        }))),
    };

    let evaluation = EEvaluation::find_by_id(build.evaluation).one(&state.db).await.unwrap().unwrap();
    let project = EProject::find_by_id(evaluation.project).one(&state.db).await.unwrap().unwrap();
    let organization = EOrganization::find_by_id(project.organization).one(&state.db).await.unwrap().unwrap();

    if organization.created_by != user.id {
        return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Build not found".to_string(),
        })));
    }

    let res = BaseResponse {
        error: false,
        message: build,
    };

    Ok(Json(res))
}

pub async fn post_build(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Path(build_id): Path<Uuid>, Json(body): Json<MakeBuildRequest>) -> Result<StreamBodyAs<'static>, (StatusCode, Json<BaseResponse<String>>)> {
    let build = match EBuild::find_by_id(build_id).one(&state.db).await.unwrap() {
        Some(b) => b,
        None => return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Build not found".to_string(),
        }))),
    };

    let evaluation = EEvaluation::find_by_id(build.evaluation).one(&state.db).await.unwrap().unwrap();
    let project = EProject::find_by_id(evaluation.project).one(&state.db).await.unwrap().unwrap();
    let organization = EOrganization::find_by_id(project.organization).one(&state.db).await.unwrap().unwrap();

    if organization.created_by != user.id {
        return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Build not found".to_string(),
        })));
    }

    let server_id = match build.server {
        Some(server) => server,
        None => return Err((StatusCode::NO_CONTENT, Json(BaseResponse {
            error: true,
            message: "Build not queued yet".to_string(),
        }))),
    };

    let server = EServer::find_by_id(server_id).one(&state.db).await.unwrap().unwrap();

    let stream = get_buildlog_stream(server, build).map_err(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(BaseResponse {
            error: true,
            message: "Failed to get build log stream".to_string(),
        }))
    }).unwrap();

    Ok(StreamBodyAs::json_array(stream))
}

pub async fn get_servers(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let servers = EServer::find()
        .join(JoinType::InnerJoin, RServer::Organization.def())
        .filter(COrganization::CreatedBy.eq(user.id))
        .all(&state.db).await.unwrap();

    let servers: ListResponse = servers.iter().map(|s| (s.id, s.name.clone())).collect();

    let res = BaseResponse {
        error: false,
        message: servers,
    };

    Ok(Json(res))
}

pub async fn post_servers(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Path(organization_id): Path<Uuid>, Json(body): Json<MakeServerRequest>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization = match EOrganization::find_by_id(organization_id).one(&state.db).await.unwrap() {
        Some(o) => o,
        None => return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Organization not found".to_string(),
        }))),
    };

    if organization.created_by != user.id {
        return Err((StatusCode::NOT_FOUND, Json(BaseResponse {
            error: true,
            message: "Organization not found".to_string(),
        })));
    }

    let server = EServer::find()
        .filter(Condition::all()
            .add(CServer::Organization.eq(organization.id))
            .add(CServer::Name.eq(body.name.clone()))
        )
        .one(&state.db)
        .await
        .unwrap();

    if server.is_some() {
        return Err((StatusCode::CONFLICT, Json(BaseResponse {
            error: true,
            message: "Server Name already exists".to_string(),
        })));
    };

    let server_id = Uuid::new_v4();
    let server = AServer {
        id: Set(server_id),
        name: Set(body.name.clone()),
        organization: Set(organization.id),
        host: Set(body.host.clone()),
        port: Set(body.port),
        username: Set(body.username.clone()),
        public_key: Set(body.public_key.clone()),
        private_key: Set(body.private_key.clone()),
        last_connection_at: Set(*NULL_TIME),
        created_by: Set(Uuid::nil()),
        created_at: Set(Utc::now().naive_utc()),
    };

    let server_architecture = AServerArchitecture {
        id: Set(Uuid::new_v4()),
        server: Set(server_id),
        architecture: Set(entity::server::Architecture::X86_64Linux),
    };

    let server = server.insert(&state.db).await.unwrap();
    server_architecture.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: server.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_user(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Path(user_id): Path<Uuid>) -> Result<Json<BaseResponse<MUser>>, (StatusCode, Json<BaseResponse<String>>)> {
    if user.id != user_id {
        return Err((StatusCode::UNAUTHORIZED, Json(BaseResponse {
            error: true,
            message: "Unauthorized".to_string(),
        })));
    }

    let user = EUser::find_by_id(user_id).one(&state.db).await.unwrap().unwrap();

    let res = BaseResponse {
        error: false,
        message: user,
    };

    Ok(Json(res))
}

pub async fn post_user(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Path(user_id): Path<Uuid>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    Err((StatusCode::NOT_IMPLEMENTED, Json(BaseResponse {
        error: true,
        message: "not implemented yet".to_string(),
    })))
}

pub async fn post_api_key(state: State<Arc<ServerState>>, Extension(user): Extension<MUser>, Json(body): Json<MakeApiKeyRequest>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let api_key = EApi::find()
        .filter(Condition::all()
            .add(CApi::OwnedBy.eq(user.id))
            .add(CApi::Name.eq(body.name.clone()))
        )
        .one(&state.db)
        .await
        .unwrap();

    if api_key.is_some() {
        return Err((StatusCode::CONFLICT, Json(BaseResponse {
            error: true,
            message: "API-Key Name already exists".to_string(),
        })));
    };

    let api_key = AApi {
        id: Set(Uuid::new_v4()),
        owned_by: Set(user.id),
        name: Set(body.name.clone()),
        key: Set(generate_api_key()),
        last_used_at: Set(*NULL_TIME),
        created_at: Set(Utc::now().naive_utc()),
    };

    let api_key = api_key.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: format!("GRAD{}", api_key.key),
    };

    Ok(Json(res))
}

pub async fn post_login(state: State<Arc<ServerState>>, Json(body): Json<MakeLoginRequest>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let user = match EUser::find()
        .filter(Condition::any()
            .add(CUser::Username.eq(body.loginname.clone()))
            .add(CUser::Email.eq(body.loginname.clone()))
        )
        .one(&state.db)
        .await
        .unwrap() {
        Some(u) => u,
        None => return Err((StatusCode::UNAUTHORIZED, Json(BaseResponse {
            error: true,
            message: "Invalid credentials".to_string(),
        }))),
    };

    verify_password(&body.password, &user.password).map_err(|_| {
        (StatusCode::UNAUTHORIZED, Json(BaseResponse {
            error: true,
            message: "Invalid credentials".to_string(),
        }))
    })?;

    let token = encode_jwt(state.clone(), user.id).map_err(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(BaseResponse {
            error: true,
            message: "Failed to generate token".to_string(),
        }))
    })?;

    update_last_login(state, user.id).await;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    Ok(Json(res))
}

pub async fn post_logout(state: State<Arc<ServerState>>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "Logout Successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_register(state: State<Arc<ServerState>>, Json(body): Json<MakeUserRequest>) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let user = EUser::find()
        .filter(Condition::any()
            .add(CUser::Username.eq(body.username.clone()))
            .add(CUser::Email.eq(body.email.clone()))
        )
        .one(&state.db)
        .await
        .unwrap();

    if user.is_some() {
        return Err((StatusCode::CONFLICT, Json(BaseResponse {
            error: true,
            message: "User already exists".to_string(),
        })));
    };

    let user = AUser {
        id: Set(Uuid::new_v4()),
        username: Set(body.username.clone()),
        name: Set(body.name.clone()),
        email: Set(body.email.clone()),
        password: Set(generate_hash(body.password.clone())),
        last_login_at: Set(*NULL_TIME),
        created_at: Set(Utc::now().naive_utc()),
    };

    let user = user.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: user.id.to_string(),
    };

    Ok(Json(res))
}
