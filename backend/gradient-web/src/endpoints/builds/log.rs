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
use gradient_db::scheduling::build_attempt::latest_attempt_id;
use gradient_types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use tokio::time::Duration;

use super::BuildAccessContext;
use super::log_chunks::{chunked_text_from, load_chunk_rows};

const POLL_INTERVAL: Duration = Duration::from_millis(500);

struct LogTail {
    attempt: Option<BuildAttemptId>,
    offset: u64,
}

impl LogTail {
    async fn appended(
        &mut self,
        state: &ServerState,
        attempt: BuildAttemptId,
    ) -> WebResult<String> {
        if self.attempt != Some(attempt) {
            *self = Self {
                attempt: Some(attempt),
                offset: 0,
            };
        }

        let logs = state.log_storage.as_ref();
        let Some(inline) = logs.read_inline_from(attempt, self.offset).await? else {
            let rows = load_chunk_rows(state, attempt).await?;
            let (text, end) = chunked_text_from(logs, attempt, &rows, self.offset).await?;
            self.offset = end;

            return Ok(text);
        };

        let (text, consumed) = complete_utf8(&inline);
        self.offset += consumed as u64;

        Ok(text)
    }
}

fn complete_utf8(bytes: &[u8]) -> (String, usize) {
    let complete = match std::str::from_utf8(bytes) {
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        _ => bytes.len(),
    };

    (
        String::from_utf8_lossy(&bytes[..complete]).into_owned(),
        complete,
    )
}

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
        let mut tail = LogTail {
            attempt: initial_attempt,
            offset: initial_offset,
        };

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let shared_build = match EDerivationBuild::find_by_id(shared_build_id).one(&state.web_db).await {
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

            match tail.appended(&state, attempt).await {
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

#[cfg(test)]
mod tests {
    use super::complete_utf8;

    #[test]
    fn complete_utf8_holds_back_a_split_character() {
        let cases: [(&str, &[u8], (&str, usize)); 4] = [
            ("whole text", "ok\n".as_bytes(), ("ok\n", 3)),
            ("split last character", &[b'a', 0xC3], ("a", 1)),
            ("invalid byte", &[b'a', 0xFF, b'b'], ("a\u{FFFD}b", 3)),
            ("nothing", &[], ("", 0)),
        ];

        for (case, bytes, (text, consumed)) in cases {
            assert_eq!(complete_utf8(bytes), (text.to_owned(), consumed), "{case}");
        }
    }
}
