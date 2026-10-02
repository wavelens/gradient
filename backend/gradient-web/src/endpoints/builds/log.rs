/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use async_stream::stream;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use axum_streams::StreamBodyAs;
use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use tokio::time::Duration;

use super::BuildAccessContext;

pub async fn get_build_log(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let log = match super::effective_log_id(&state, &ctx.shared_build).await {
        Some(key) => state.log_storage.read(key).await.unwrap_or_default(),
        None => String::new(),
    };

    Ok(ok_json(log))
}

pub async fn post_build_log(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> Result<Response, WebError> {
    let ctx = BuildAccessContext::load(&state, build_id, &Some(user), api_key.as_ref()).await?;
    let shared_build_id = ctx.shared_build.id;

    let initial_log_key =
        gradient_db::scheduling::build_attempt::latest_attempt_id(&state.web_db, shared_build_id)
            .await?;
    let initial_offset = match initial_log_key {
        Some(key) => state.log_storage.read(key).await.unwrap_or_default().len(),
        None => 0,
    };

    let stream = stream! {
        use gradient_entity::build::BuildStatus;

        let mut last_offset: usize = initial_offset;
        let mut sent_any: bool = false;

        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;

            let shared_build = match EDerivationBuild::find_by_id(shared_build_id).one(&state.web_db).await {
                Ok(Some(a)) => a,
                Ok(None) => break,
                Err(_) => break,
            };
            let Some(log_key) = gradient_db::scheduling::build_attempt::latest_attempt_id(&state.web_db, shared_build_id).await.unwrap_or(None) else {
                if matches!(shared_build.status, BuildStatus::Created | BuildStatus::Queued) {
                    continue;
                }
                if !sent_any {
                    yield String::new();
                }
                break;
            };

            // A build that is not executing yet must keep the stream open. A UI that opened the
            // stream before a worker picked the build up would otherwise see an empty response.
            if matches!(shared_build.status, BuildStatus::Created | BuildStatus::Queued) {
                continue;
            }

            let log = state.log_storage.read(log_key).await.unwrap_or_default();
            if log.len() > last_offset {
                let log_new = log[last_offset..].to_string();
                last_offset = log.len();
                if !log_new.is_empty() {
                    sent_any = true;
                    yield log_new;
                }
            }

            // Any status other than `Building` is terminal. A final read is catching lines appended
            // between the read above and the status transition commit.
            if shared_build.status != BuildStatus::Building {
                let final_log = state.log_storage.read(log_key).await.unwrap_or_default();
                if final_log.len() > last_offset {
                    let final_chunk = final_log[last_offset..].to_string();
                    if !final_chunk.is_empty() {
                        sent_any = true;
                        yield final_chunk;
                    }
                }
                if !sent_any {
                    yield String::new();
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
