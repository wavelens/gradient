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
    let log = match super::effective_log_id(&state, &ctx.anchor).await {
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
    let anchor_id = ctx.anchor.id;

    // Capture current log length so the stream only delivers new content,
    // avoiding duplication of what the client already received via GET.
    let initial_log_key = gradient_db::latest_attempt_id(&state.web_db, anchor_id).await?;
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

            let anchor = match EDerivationBuild::find_by_id(anchor_id).one(&state.web_db).await {
                Ok(Some(a)) => a,
                Ok(None) => break,
                Err(_) => break,
            };
            let Some(log_key) = gradient_db::latest_attempt_id(&state.web_db, anchor_id).await.unwrap_or(None) else {
                if matches!(anchor.status, BuildStatus::Created | BuildStatus::Queued) {
                    continue;
                }
                if !sent_any {
                    yield String::new();
                }
                break;
            };

            // While the build hasn't started executing yet (`Created` /
            // `Queued`), there's nothing to stream - but we must not close
            // the connection either, otherwise a UI that opened the stream
            // before the worker picked the build up would see an empty
            // response and never get the live output. Keep polling.
            if matches!(anchor.status, BuildStatus::Created | BuildStatus::Queued) {
                continue;
            }

            // Building / terminal: read whatever's in the log buffer so far
            // and emit only the new tail.
            let log = state.log_storage.read(log_key).await.unwrap_or_default();
            if log.len() > last_offset {
                let log_new = log[last_offset..].to_string();
                last_offset = log.len();
                if !log_new.is_empty() {
                    sent_any = true;
                    yield log_new;
                }
            }

            // Anything other than `Building` is terminal - flush a final
            // read (catches the race where lines were appended between our
            // read above and the daemon-side status transition committing)
            // and close the stream.
            if anchor.status != BuildStatus::Building {
                let final_log = state.log_storage.read(log_key).await.unwrap_or_default();
                if final_log.len() > last_offset {
                    let final_chunk = final_log[last_offset..].to_string();
                    if !final_chunk.is_empty() {
                        sent_any = true;
                        yield final_chunk;
                    }
                }
                if !sent_any {
                    // Build completed (or was Substituted / DependencyFailed
                    // and never produced output) - emit one empty frame so
                    // the client sees a clean end-of-stream rather than a
                    // hanging connection.
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
