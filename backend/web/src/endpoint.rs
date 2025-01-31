/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use async_stream::stream;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use axum_streams::*;
use builder::scheduler::abort_evaluation;
use chrono::Utc;
use core::consts::*;
use core::database::add_features;
use core::executer::connect;
use core::input::{valid_evaluation_wildcard, vec_to_hex};
use core::sources::*;
use core::types::*;
use email_address::EmailAddress;
use git_url_parse::normalize_url;
use password_auth::{generate_hash, verify_password};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter, QuerySelect,
    RelationTrait,
};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::auth::{encode_jwt, generate_api_key, update_last_login};
use super::requests::*;

// TODO: sanitize inputs

pub async fn handle_404() -> (StatusCode, Json<BaseResponse<String>>) {
    (
        StatusCode::NOT_FOUND,
        Json(BaseResponse {
            error: true,
            message: "Not Found".to_string(),
        }),
    )
}

pub async fn get_health(
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let res = BaseResponse {
        error: false,
        message: "200 ALIVE".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_organizations(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organizations = EOrganization::find()
        .filter(COrganization::CreatedBy.eq(user.id))
        .all(&state.db)
        .await
        .unwrap();

    let organizations: ListResponse = organizations
        .iter()
        .map(|o| ListItem {
            id: o.id,
            name: o.name.clone(),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: organizations,
    };

    Ok(Json(res))
}

pub async fn post_organizations(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<MakeOrganizationRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let (private_key, public_key) =
        generate_ssh_key(state.cli.crypt_secret.clone()).map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: "Failed to generate SSH key".to_string(),
                }),
            )
        })?;

    let organization = AOrganization {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        description: Set(body.description.clone()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        use_nix_store: Set(body.use_nix_store),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let organization = organization.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: organization.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_organization(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization_id): Path<Uuid>,
) -> Result<Json<BaseResponse<MOrganization>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization = match EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await
        .unwrap()
    {
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

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Organization not found".to_string(),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: organization,
    };

    Ok(Json(res))
}

pub async fn post_organization(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization_id): Path<Uuid>,
    Json(body): Json<MakeProjectRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let repository_url = normalize_url(body.repository.clone().as_str()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Repository URL".to_string(),
            }),
        )
    })?;

    let organization = match EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await
        .unwrap()
    {
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

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Organization not found".to_string(),
            }),
        ));
    }

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
        description: Set(body.description.clone()),
        repository: Set(repository_url.to_string()),
        evaluation_wildcard: Set(body.evaluation_wildcard.clone()),
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

pub async fn get_organization_ssh(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization_id): Path<Uuid>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization = match EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await
        .unwrap()
    {
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

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Organization not found".to_string(),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: format_public_key(organization),
    };

    Ok(Json(res))
}

pub async fn post_organization_ssh(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization_id): Path<Uuid>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization = match EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await
        .unwrap()
    {
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

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Organization not found".to_string(),
            }),
        ));
    }

    let (private_key, public_key) = generate_ssh_key(state.cli.crypt_secret.clone()).unwrap();

    let mut aorganization: AOrganization = organization.into();

    aorganization.private_key = Set(private_key.clone());
    aorganization.public_key = Set(public_key.clone());
    let organization = aorganization.update(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: format_public_key(organization),
    };

    Ok(Json(res))
}

pub async fn get_organization_projects(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization_id): Path<Uuid>,
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization = match EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await
        .unwrap()
    {
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

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Organization not found".to_string(),
            }),
        ));
    }

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

pub async fn get_project(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<BaseResponse<MProject>>, (StatusCode, Json<BaseResponse<String>>)> {
    let project = match EProject::find_by_id(project_id)
        .one(&state.db)
        .await
        .unwrap()
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

    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Project not found".to_string(),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: project,
    };

    Ok(Json(res))
}

pub async fn post_project(
    _state: State<Arc<ServerState>>,
    Extension(_user): Extension<MUser>,
    Path(_project_id): Path<Uuid>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(BaseResponse {
            error: true,
            message: "not implemented yet".to_string(),
        }),
    ))
    // let res = BaseResponse {
    //     error: false,
    //     message: "Project configured successfully".to_string(),
    // };

    // Ok(Json(res))
}

pub async fn post_project_check_repository(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let project = match EProject::find_by_id(project_id)
        .one(&state.db)
        .await
        .unwrap()
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

    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Project not found".to_string(),
            }),
        ));
    }

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

pub async fn get_evaluation(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(evaluation_id): Path<Uuid>,
) -> Result<Json<BaseResponse<MEvaluation>>, (StatusCode, Json<BaseResponse<String>>)> {
    let evaluation = match EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await
        .unwrap()
    {
        Some(e) => e,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Evaluation not found".to_string(),
                }),
            ))
        }
    };

    let project = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Evaluation not found".to_string(),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: evaluation,
    };

    Ok(Json(res))
}

pub async fn post_evaluation(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(evaluation_id): Path<Uuid>,
    Json(body): Json<MakeEvaluationRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let evaluation = match EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await
        .unwrap()
    {
        Some(e) => e,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Evaluation not found".to_string(),
                }),
            ))
        }
    };

    let project = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Evaluation not found".to_string(),
            }),
        ));
    }

    if body.method == "abort" {
        abort_evaluation(Arc::clone(&state), evaluation).await;
    }

    let res = BaseResponse {
        error: false,
        message: "Success".to_string(),
    };

    Ok(Json(res))
}

pub async fn connect_evaluation(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(evaluation_id): Path<Uuid>,
) -> Result<StreamBodyAs<'static>, (StatusCode, Json<BaseResponse<String>>)> {
    let evaluation = match EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await
        .unwrap()
    {
        Some(e) => e,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Evaluation not found".to_string(),
                }),
            ))
        }
    };

    let project = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Evaluation not found".to_string(),
            }),
        ));
    }

    let condition = Condition::all()
        .add(CBuild::Evaluation.eq(evaluation.id))
        .add(CBuild::Status.eq(entity::build::BuildStatus::Building));

    let stream = stream! {
        let mut last_logs: HashMap<Uuid, String> = HashMap::new();
        loop {
            let builds = EBuild::find()
                .filter(condition.clone())
                .all(&state.db)
                .await
                .unwrap();

            if builds.is_empty() {
                break;
            }

            for build in builds {
                let name = build.derivation_path.split("-").next().unwrap();
                let name = build.derivation_path.replace(format!("{}-", name).as_str(), "").replace(".drv", "");
                let log = build.log.unwrap_or("".to_string())
                    .split("\n")
                    .map(|l| format!("{}> {}", name, l))
                    .collect::<Vec<String>>()
                    .join("\n");

                if last_logs.contains_key(&build.id) {
                    let last_log = last_logs.get(&build.id).unwrap();
                    let log_new = log.replace(last_log.as_str(), "");
                    if !log_new.is_empty() {
                        last_logs.insert(build.id, log.clone());
                        yield log_new.clone();
                    }
                } else {
                    last_logs.insert(build.id, log.clone());
                }
            }
        }
    };

    Ok(StreamBodyAs::json_nl(stream))
}

pub async fn get_builds(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(evaluation_id): Path<Uuid>,
) -> Result<Json<BaseResponse<Vec<String>>>, (StatusCode, Json<BaseResponse<String>>)> {
    let evaluation = match EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await
        .unwrap()
    {
        Some(e) => e,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Evaluation not found".to_string(),
                }),
            ))
        }
    };

    let project = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Evaluation not found".to_string(),
            }),
        ));
    }

    let builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .all(&state.db)
        .await
        .unwrap();

    let builds: Vec<String> = builds.iter().map(|b| b.id.to_string()).collect();

    let res = BaseResponse {
        error: false,
        message: builds,
    };

    Ok(Json(res))
}

pub async fn get_build(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(build_id): Path<Uuid>,
) -> Result<Json<BaseResponse<MBuild>>, (StatusCode, Json<BaseResponse<String>>)> {
    let build = match EBuild::find_by_id(build_id).one(&state.db).await.unwrap() {
        Some(b) => b,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Build not found".to_string(),
                }),
            ))
        }
    };

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let project = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Build not found".to_string(),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: build,
    };

    Ok(Json(res))
}

pub async fn connect_build(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(build_id): Path<Uuid>,
) -> Result<StreamBodyAs<'static>, (StatusCode, Json<BaseResponse<String>>)> {
    let build = match EBuild::find_by_id(build_id).one(&state.db).await.unwrap() {
        Some(b) => b,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Build not found".to_string(),
                }),
            ))
        }
    };

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let project = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Build not found".to_string(),
            }),
        ));
    }

    // TODO: check if build is building

    // watch build.log and stream it

    let stream = stream! {
        let mut last_log = build.log.unwrap_or("".to_string());
        if !last_log.is_empty() {
            // TODO: Chunkify past log
            yield last_log.clone();
        }

        loop {
            let build = EBuild::find_by_id(build_id).one(&state.db).await.unwrap().unwrap();
            if build.status != entity::build::BuildStatus::Building {
                break;
            }

            let log = build.log.unwrap_or("".to_string());
            let log_new = log.replace(last_log.as_str(), "");
            if !log_new.is_empty() {
                last_log = log.clone();
                yield log_new.clone();
            }
        }
    };

    Ok(StreamBodyAs::json_nl(stream))
}

pub async fn get_servers(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<MakeServerGetRequest>,
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: check if user is authorized to access the organization
    let servers = EServer::find()
        .join(JoinType::InnerJoin, RServer::Organization.def())
        .filter(COrganization::CreatedBy.eq(user.id))
        .filter(CServer::Organization.eq(body.organization_id))
        .all(&state.db)
        .await
        .unwrap();

    let servers: ListResponse = servers
        .iter()
        .map(|s| ListItem {
            id: s.id,
            name: s.name.clone(),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: servers,
    };

    Ok(Json(res))
}

pub async fn post_servers(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<MakeServerRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization_id = match Uuid::parse_str(&body.organization_id) {
        Ok(id) => id,
        Err(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid Organization ID".to_string(),
                }),
            ))
        }
    };

    let organization = match EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await
        .unwrap()
    {
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

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Organization not found".to_string(),
            }),
        ));
    }

    let server = EServer::find()
        .filter(
            Condition::all()
                .add(CServer::Organization.eq(organization.id))
                .add(CServer::Name.eq(body.name.clone())),
        )
        .one(&state.db)
        .await
        .unwrap();

    if server.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "Server Name already exists".to_string(),
            }),
        ));
    };

    let server = AServer {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        organization: Set(organization.id),
        host: Set(body.host.clone()),
        port: Set(body.port),
        username: Set(body.username.clone()),
        last_connection_at: Set(*NULL_TIME),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let architectures = body
        .architectures
        .iter()
        .map(|a| entity::server::Architecture::try_from(a.as_str()))
        .filter_map(|a| a.ok())
        .collect::<Vec<entity::server::Architecture>>();

    if architectures.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Architectures".to_string(),
            }),
        ));
    }

    let server = server.insert(&state.db).await.unwrap();
    let server_architecture = architectures
        .iter()
        .map(|a| AServerArchitecture {
            id: Set(Uuid::new_v4()),
            server: Set(server.id),
            architecture: Set(a.clone()),
        })
        .collect::<Vec<AServerArchitecture>>();

    add_features(
        Arc::clone(&state),
        body.features.clone(),
        None,
        Some(server.id),
    )
    .await;

    EServerArchitecture::insert_many(server_architecture)
        .exec(&state.db)
        .await
        .unwrap();

    let res = BaseResponse {
        error: false,
        message: server.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn post_server_check(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(server_id): Path<Uuid>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let server = match EServer::find_by_id(server_id).one(&state.db).await.unwrap() {
        Some(s) => s,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Server not found".to_string(),
                }),
            ))
        }
    };

    let organization = EOrganization::find_by_id(server.organization)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    if organization.created_by != user.id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Server not found".to_string(),
            }),
        ));
    }

    let (private_key, public_key) =
        decrypt_ssh_private_key(state.cli.crypt_secret.clone(), organization.clone()).unwrap();

    match connect(server, None, public_key, private_key).await {
        Ok(_) => Ok(Json(BaseResponse {
            error: false,
            message: "server connection established".to_string(),
        })),
        Err(e) => Err((
            StatusCode::GATEWAY_TIMEOUT,
            Json(BaseResponse {
                error: true,
                message: format!("server connection failed with error: {}", e),
            }),
        )),
    }
}

pub async fn get_user(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(user_id): Path<Uuid>,
) -> Result<Json<BaseResponse<MUser>>, (StatusCode, Json<BaseResponse<String>>)> {
    if user.id != user_id {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(BaseResponse {
                error: true,
                message: "Unauthorized".to_string(),
            }),
        ));
    }

    let user = EUser::find_by_id(user_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    let res = BaseResponse {
        error: false,
        message: user,
    };

    Ok(Json(res))
}

pub async fn post_user(
    _state: State<Arc<ServerState>>,
    Extension(_user): Extension<MUser>,
    Path(_user_id): Path<Uuid>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(BaseResponse {
            error: true,
            message: "not implemented yet".to_string(),
        }),
    ))
}

pub async fn post_api_key(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<MakeApiKeyRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let api_key = EApi::find()
        .filter(
            Condition::all()
                .add(CApi::OwnedBy.eq(user.id))
                .add(CApi::Name.eq(body.name.clone())),
        )
        .one(&state.db)
        .await
        .unwrap();

    if api_key.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "API-Key Name already exists".to_string(),
            }),
        ));
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

pub async fn get_user_info(
    Extension(user): Extension<MUser>,
) -> Result<Json<BaseResponse<UserInfoResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let user_info = UserInfoResponse {
        id: user.id.to_string(),
        username: user.username.clone(),
        name: user.name.clone(),
        email: user.email.clone(),
    };

    let res = BaseResponse {
        error: false,
        message: user_info,
    };

    Ok(Json(res))
}

pub async fn post_login(
    state: State<Arc<ServerState>>,
    Json(body): Json<MakeLoginRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let user = match EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Username.eq(body.loginname.clone()))
                .add(CUser::Email.eq(body.loginname.clone())),
        )
        .one(&state.db)
        .await
        .unwrap()
    {
        Some(u) => u,
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid credentials".to_string(),
                }),
            ))
        }
    };

    verify_password(&body.password, &user.password).map_err(|_| {
        (
            StatusCode::UNAUTHORIZED,
            Json(BaseResponse {
                error: true,
                message: "Invalid credentials".to_string(),
            }),
        )
    })?;

    let token = encode_jwt(state.clone(), user.id).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BaseResponse {
                error: true,
                message: "Failed to generate token".to_string(),
            }),
        )
    })?;

    update_last_login(state, user.id).await;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    Ok(Json(res))
}

pub async fn post_logout(
    _state: State<Arc<ServerState>>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: invalidate token if needed
    let res = BaseResponse {
        error: false,
        message: "Logout Successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_register(
    state: State<Arc<ServerState>>,
    Json(body): Json<MakeUserRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    if !EmailAddress::is_valid(body.email.clone().as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Email".to_string(),
            }),
        ));
    }

    let user = EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Username.eq(body.username.clone()))
                .add(CUser::Email.eq(body.email.clone())),
        )
        .one(&state.db)
        .await
        .unwrap();

    if user.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "User already exists".to_string(),
            }),
        ));
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
