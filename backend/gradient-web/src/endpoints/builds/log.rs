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
use gradient_db::lookup::by_id;
use gradient_db::scheduling::build_attempt::latest_attempt_id;
use gradient_db::status::log_tail::LogTail;
use gradient_types::*;
use std::sync::Arc;
use tokio::time::Duration;

use super::BuildAccessContext;

const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[utoipa::path(
    get,
    path = "/builds/{build}/log",
    operation_id = "getBuildLog",
    tag = "builds",
    summary = "Get build log",
    description = "Returns the complete build log as a single string. Empty string if no log is available yet.\n\nA `Substituted` build (its outputs came from a cache) has no log of its\nown, so the log of the most recent prior build of the same derivation is\nreturned instead. The chunk/lines endpoints fall back the same way.\n\n**Access:** Accessible to members of any project whose evaluation references this derivation (a derivation is built once and shared across projects via cache reachability). See `docs/src/concepts/overview.md#shared-builds`.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Build log", body = String, content_type = "application/json"), WebError)
)]
pub async fn get_build_log(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let log = match super::effective_log_id(&state.web_db, &ctx.shared_build).await {
        Some(key) => state.log_storage.read(key).await.unwrap_or_default(),
        None => String::new(),
    };

    Ok(ok_json(log))
}

#[utoipa::path(
    post,
    path = "/builds/{build}/log",
    operation_id = "streamBuildLog",
    tag = "builds",
    summary = "Stream build log",
    description = "Open a streaming NDJSON response: emit the full existing log first, then\nstream new chunks as they arrive. Close the stream once the build has left\nthe `Building` state. Chunks hold no more than 256 KiB each, split at a\nline end. Empty strings arrive as keepalives after 15 seconds without\noutput. Ignore them.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "NDJSON log stream", body = String, content_type = "application/x-ndjson"), WebError)
)]
pub async fn post_build_log(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> Result<Response, WebError> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &Some(user), api_key.as_ref()).await?;
    let shared_build_id = ctx.shared_build.id;

    let initial_attempt = latest_attempt_id(&state.web_db, shared_build_id).await?;
    let initial_offset = match initial_attempt {
        Some(attempt) => state
            .log_storage
            .read(attempt)
            .await
            .unwrap_or_default()
            .len() as u64,
        None => 0,
    };

    let stream = stream! {
        use gradient_entity::build::BuildStatus;

        let mut sent_any = false;
        let mut tail = LogTail::after(initial_attempt, initial_offset);

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let shared_build = match by_id::<EDerivationBuild, _>(&state.web_db, shared_build_id).await {
                Ok(Some(shared_build)) => shared_build,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(%shared_build_id, error = %e, "build log stream could not load the build");
                    break;
                }
            };

            if matches!(shared_build.status, BuildStatus::Created | BuildStatus::Queued) {
                continue;
            }

            let Ok(Some(attempt)) = latest_attempt_id(&state.web_db, shared_build_id).await else {
                break;
            };

            match tail.appended(&state.web_db, state.log_storage.as_ref(), attempt).await {
                Ok(text) if text.is_empty() => {}
                Ok(text) => {
                    sent_any = true;
                    yield text;
                }
                Err(e) => {
                    tracing::warn!(%shared_build_id, error = %e, "build log stream could not read the log");
                    break;
                }
            }

            if shared_build.status != BuildStatus::Building {
                break;
            }
        }

        if !sent_any {
            yield String::new();
        }
    };

    let mut response =
        StreamBodyAs::json_nl(crate::endpoints::log_stream::keepalive_chunks(stream))
            .into_response();

    response
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));

    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));

    Ok(response)
}
