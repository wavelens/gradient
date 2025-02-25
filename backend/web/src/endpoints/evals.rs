/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use async_stream::stream;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use axum_streams::StreamBodyAs;
use builder::scheduler::abort_evaluation;
use core::types::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeEvaluationRequest {
    pub method: String,
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

pub async fn get_evaluation_builds(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(evaluation_id): Path<Uuid>,
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
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

    let builds: ListResponse = builds
        .iter()
        .map(|b| ListItem {
            id: b.id,
            name: b.derivation_path.clone(),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: builds,
    };

    Ok(Json(res))
}

pub async fn connect_evaluation_builds(
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
        let mut no_response: i16 = 0;
        let mut first_response: bool = true;
        while no_response < 5 {
            let builds = EBuild::find()
                .filter(condition.clone())
                .all(&state.db)
                .await
                .unwrap();

            if builds.is_empty() {
                if first_response {
                    yield "".to_string();
                    break;
                }

                no_response += 1;
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }

            first_response = false;

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
