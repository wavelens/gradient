/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use async_stream::stream;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use axum_streams::StreamBodyAs;
use core::types::*;
use sea_orm::EntityTrait;
use sea_orm::{ColumnTrait, QueryFilter};
use std::sync::Arc;
use tokio::time::Duration;
use uuid::Uuid;

pub async fn get_build_log(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<String>>> {
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

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation.id
                );
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;
        project.organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("DirectBuild not found for evaluation {}", evaluation.id);
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization
    };

    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!("Organization {} not found", organization_id);
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    let can_access = if organization.public {
        true
    } else {
        match &maybe_user {
            Some(user) => user_is_org_member(&state, user.id, organization.id).await?,
            None => false,
        }
    };
    if !can_access {
        return Err(WebError::not_found("Build"));
    }

    let log_key = build.log_id.unwrap_or(build_id);
    let log = state.log_storage.read(log_key).await.unwrap_or_default();
    let res = BaseResponse {
        error: false,
        message: log,
    };

    Ok(Json(res))
}

pub async fn post_build_log(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(build_id): Path<Uuid>,
) -> Result<Response, WebError> {
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
    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation.id
                );
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;
        project.organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("DirectBuild not found for evaluation {}", evaluation.id);
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization
    };
    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!("Organization {} not found", organization_id);
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    if !user_is_org_member(&state, user.id, organization.id).await? {
        return Err(WebError::not_found("Build"));
    }

    // Capture current log length so the stream only delivers new content,
    // avoiding duplication of what the client already received via GET.
    let log_key = build.log_id.unwrap_or(build_id);
    let initial_offset = state
        .log_storage
        .read(log_key)
        .await
        .unwrap_or_default()
        .len();

    let stream = stream! {
        let mut last_offset: usize = initial_offset;
        let mut sent_any: bool = false;

        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;

            let build = match EBuild::find_by_id(build_id).one(&state.db).await {
                Ok(Some(b)) => b,
                Ok(None) => break,
                Err(_) => break,
            };

            let log = state.log_storage.read(build.log_id.unwrap_or(build_id)).await.unwrap_or_default();
            let log_new = log[last_offset..].to_string();

            if !log_new.is_empty() {
                sent_any = true;
                last_offset = log.len();
                yield log_new;
            }

            if build.status != entity::build::BuildStatus::Building {
                // One extra read: catches log lines flushed between our read
                // above and the status transition being committed.
                let final_log = state.log_storage.read(build.log_id.unwrap_or(build_id)).await.unwrap_or_default();
                let final_chunk = final_log[last_offset..].to_string();
                if !final_chunk.is_empty() {
                    sent_any = true;
                    yield final_chunk;
                }
                if !sent_any {
                    yield "".to_string();
                }
                break;
            }
        }
    };

    let mut response = StreamBodyAs::json_nl(stream).into_response();
    response
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(response)
}
