/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use async_stream::stream;
use axum::extract::{Path, State};
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
) -> WebResult<Json<BaseResponse<MBuild>>> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Evaluation {} not found for build {}",
                build.evaluation,
                build_id
            );
            WebError::InternalServerError("Build data inconsistency".to_string())
        })?;
    let project = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Project {} not found for evaluation {}",
                evaluation.project,
                evaluation.id
            );
            WebError::InternalServerError("Evaluation data inconsistency".to_string())
        })?;
    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Organization {} not found for project {}",
                project.organization,
                project.id
            );
            WebError::InternalServerError("Project data inconsistency".to_string())
        })?;

    if organization.created_by != user.id {
        return Err(WebError::not_found("Build"));
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
) -> Result<StreamBodyAs<'static>, WebError> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Evaluation {} not found for build {}",
                build.evaluation,
                build_id
            );
            WebError::InternalServerError("Build data inconsistency".to_string())
        })?;
    let project = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Project {} not found for evaluation {}",
                evaluation.project,
                evaluation.id
            );
            WebError::InternalServerError("Evaluation data inconsistency".to_string())
        })?;
    let organization = EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Organization {} not found for project {}",
                project.organization,
                project.id
            );
            WebError::InternalServerError("Project data inconsistency".to_string())
        })?;

    if organization.created_by != user.id {
        return Err(WebError::not_found("Build"));
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
