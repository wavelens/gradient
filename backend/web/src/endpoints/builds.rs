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
use core::types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use uuid::Uuid;

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

pub async fn post_build(
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
        let mut first_response: bool = true;
        if !last_log.is_empty() {
            // TODO: Chunkify past log
            first_response = false;
            yield last_log.clone();
        }

        loop {
            let build = EBuild::find_by_id(build_id).one(&state.db).await.unwrap().unwrap();
            if build.status != entity::build::BuildStatus::Building {
                if first_response {
                    yield "".to_string();
                }

                break;
            }

            first_response = false;

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
