/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use async_stream::stream;
use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum_streams::StreamBodyAs;
use gradient_core::ServerState;
use gradient_db::lookup::by_id;
use gradient_db::scheduling::build_attempt::latest_attempt_id;
use gradient_db::status::log_tail::LogTail;
use gradient_types::*;
use serde::Deserialize;
use std::sync::Arc;
use tokio::time::Duration;

use super::BuildAccessContext;

const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BuildLogAttemptQuery {
    /// The attempt to read logs from, as listed in `GET /builds/{build}/attempts`. The default is the latest attempt.
    pub attempt: Option<BuildAttemptId>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum BuildLogOutputFormat {
    Ansi,
    Plain,
}

#[derive(Deserialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BuildLogQuery {
    /// The attempt to read logs from, as listed in `GET /builds/{build}/attempts`. The default is the latest attempt.
    attempt: Option<BuildAttemptId>,
    format: Option<BuildLogOutputFormat>,
}

#[utoipa::path(
    get,
    path = "/builds/{build}/log",
    operation_id = "getBuildLog",
    tag = "builds",
    summary = "Get build log",
    description = "Return the full build log. Return an empty string when no log is present yet. Use a query parameter, `format`, to choose `plain` (remove color codes) or `ansi` (keep color codes). Requests that specify `Accept: text/plain` get the raw text of the log. The default value is `plain`. Other requests get the log as a JSON string. The default value is `ansi`. Logs of failed builds end with `Full log: <url>`. That line is a link to the web page of this build. That line is a replacement for the `nix log` command from nix. That command can run only on the server.\n\nA `Substituted` build (its outputs came from a cache) has no log of its\nown, so the log of the most recent prior build of the same derivation is\nreturned instead. The chunk/lines endpoints fall back the same way.\n\n**Access:** Accessible to members of any project whose evaluation references this derivation (a derivation is built once and shared across projects via cache reachability). See `docs/src/concepts/overview.md#shared-builds`.",
    params(("build" = BuildJobId, Path), BuildLogQuery),
    responses(
        (status = OK, description = "Build log", content(
            (String = "application/json"),
            (String = "text/plain"),
        )),
        WebError
    )
)]
pub async fn get_build_log(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
    Query(query): Query<BuildLogQuery>,
    headers: HeaderMap,
) -> WebResult<Response> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let mut log = match super::build_attempt_id_for_log(&state.web_db, &ctx.shared_build, query.attempt)
        .await?
    {
        Some(key) => state.log_storage.read(key).await?,
        None => String::new(),
    };

    if ctx.shared_build.status.is_failure() {
        let link = frontend_page_urls::build_log(
            &state.config.server.frontend_url,
            &ctx.project.name,
            ctx.build_job.evaluation,
            build_id,
        );
        log = gradient_sources::replace_nix_log_pointer_with_web_log_link(&log, &link);
    }

    let wants_text = accepts_plain_text(&headers);
    let format = query.format.unwrap_or(if wants_text {
        BuildLogOutputFormat::Plain
    } else {
        BuildLogOutputFormat::Ansi
    });
    if format == BuildLogOutputFormat::Plain {
        log = gradient_sources::strip_ansi_escape_sequences(&log);
    }

    Ok(if wants_text {
        ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], log).into_response()
    } else {
        ok_json(log).into_response()
    })
}

fn accepts_plain_text(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|media| {
            media
                .split(';')
                .next()
                .is_some_and(|m| m.trim() == "text/plain")
        })
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

    let stream = stream! {
        use gradient_entity::build::BuildStatus;

        let mut sent_any = false;
        let mut tail = LogTail::default();

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
