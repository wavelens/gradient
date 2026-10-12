/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::BuildReadRefusal;
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;
use async_stream::stream;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use axum_streams::StreamBodyAs;
use gradient_core::ServerState;
use gradient_storage::sgr::SgrState;
use gradient_types::*;
use serde::Deserialize;
use std::sync::Arc;

use super::BuildAccessContext;

type ChunkRow = gradient_entity::build_log_chunk::Model;

fn decode_chunk(raw: &[u8]) -> WebResult<String> {
    let bytes =
        zstd::stream::decode_all(raw).map_err(|e| WebError::Internal(anyhow::Error::new(e)))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn chunk_lines_text(
    logs: &dyn gradient_storage::LogStorage,
    log_key: BuildAttemptId,
    row: &ChunkRow,
) -> WebResult<String> {
    Ok(gradient_storage::log_chunk::read_chunk_text(
        logs,
        log_key,
        row.chunk_index as u32,
        row.line_count.max(0) as u32,
    )
    .await?)
}

#[utoipa::path(
    get,
    path = "/builds/{build}/log/chunks",
    operation_id = "getBuildLogChunks",
    tag = "builds",
    summary = "Get build log chunk index",
    description = "Returns the index of the zstd-compressed log chunks written when the build\nfinalized: total chunk/line/byte counts and per-chunk line/byte offsets.\n`total_chunks` is `0` for builds whose log has not been finalized yet (use the\nstreaming `POST /log` endpoint for in-progress builds).\n\n**Access:** Same as `GET /builds/{build}/log`.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Chunk index", body = LogChunkIndex), WebError)
)]
pub async fn get_build_log_chunks(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<LogChunkIndex>>> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let rows = match super::effective_log_id(&state.web_db, &ctx.shared_build).await {
        Some(log_key) => gradient_db::build_log_chunks::of_attempt(&state.web_db, log_key).await?,
        None => vec![],
    };

    let chunks: Vec<LogChunkMeta> = rows
        .iter()
        .map(|r| LogChunkMeta {
            index: r.chunk_index as u32,
            line_start: r.line_start as u64,
            line_count: r.line_count as u32,
            byte_start: r.byte_start as u64,
            byte_len: r.byte_len as u32,
        })
        .collect();

    let total_lines = rows
        .last()
        .map(|r| r.line_start as u64 + r.line_count as u64)
        .unwrap_or(0);

    let total_bytes = rows
        .last()
        .map(|r| r.byte_start as u64 + r.byte_len as u64)
        .unwrap_or(0);

    Ok(ok_json(LogChunkIndex {
        total_chunks: u32::try_from(chunks.len()).unwrap_or(u32::MAX),
        total_lines,
        total_bytes,
        chunks,
    }))
}

#[utoipa::path(
    get,
    path = "/builds/{build}/log/chunk/{index}",
    operation_id = "getBuildLogChunk",
    tag = "builds",
    summary = "Get a single decompressed log chunk",
    description = "Returns one chunk's plaintext (server-decompressed) with the active ANSI\ncolor state at the chunk boundary already prepended, so the chunk renders\ncorrectly standalone.\n\n**Access:** Same as `GET /builds/{build}/log`.",
    params(("build" = BuildJobId, Path), ("index" = u32, Path)),
    responses((status = OK, description = "Chunk plaintext", body = String, content_type = "text/plain"), WebError)
)]
pub async fn get_build_log_chunk(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((build_id, index)): Path<(BuildJobId, u32)>,
) -> Result<Response, WebError> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let log_key = super::effective_log_id(&state.web_db, &ctx.shared_build)
        .await
        .ok_or_else(|| WebError::not_found(Resource::LogChunk))?;

    let row = gradient_db::build_log_chunks::by_index(&state.web_db, log_key, index as i32)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::LogChunk))?;

    let raw = state
        .log_storage
        .read_chunk(log_key, index)
        .await
        .map_err(|_| WebError::not_found(Resource::LogChunk))?;

    let text = format!("{}{}", row.color_prefix, decode_chunk(&raw)?);

    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LineRangeQuery {
    pub start: Option<u64>,
    pub end: Option<u64>,
    pub range: Option<String>,
}

fn parse_line_range(q: &LineRangeQuery) -> Result<(u64, Option<u64>), WebError> {
    if let Some(range) = &q.range {
        let cleaned = range.replace(['L', 'l'], "");
        let (lo, hi) = cleaned
            .split_once('-')
            .ok_or(BuildReadRefusal::MalformedRange)?;

        let start: u64 = lo
            .trim()
            .parse()
            .map_err(|_| BuildReadRefusal::InvalidRangeBound("start"))?;

        let end: u64 = hi
            .trim()
            .parse()
            .map_err(|_| BuildReadRefusal::InvalidRangeBound("end"))?;

        return Ok((start.max(1), Some(end)));
    }
    Ok((q.start.unwrap_or(1).max(1), q.end))
}

#[utoipa::path(
    get,
    path = "/builds/{build}/log/lines",
    operation_id = "getBuildLogLines",
    tag = "builds",
    summary = "Get a build log line range",
    description = "Returns the plaintext for an inclusive 1-based logical line range, resolving\nthe covering chunk(s) server-side and prepending the correct color prefix for\nthe first line. Hand- and CLI-friendly; the frontend also uses this as its\nlazy fetch path.\n\n**Access:** Same as `GET /builds/{build}/log`.",
    params(("build" = BuildJobId, Path), LineRangeQuery),
    responses((status = OK, description = "Line range plaintext", body = String, content_type = "text/plain"), WebError)
)]
pub async fn get_build_log_lines(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
    Query(q): Query<LineRangeQuery>,
) -> Result<Response, WebError> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let Some(log_key) = super::effective_log_id(&state.web_db, &ctx.shared_build).await else {
        return Ok((
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            String::new(),
        )
            .into_response());
    };

    let rows = gradient_db::build_log_chunks::of_attempt(&state.web_db, log_key).await?;

    let (start, end_opt) = parse_line_range(&q)?;
    let total_lines = rows
        .last()
        .map(|r| r.line_start as u64 + r.line_count as u64)
        .unwrap_or(0);
    let end = end_opt.unwrap_or(total_lines).min(total_lines);

    let mut out = String::new();
    if total_lines == 0 || start > end {
        return Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], out).into_response());
    }

    let want_first = start - 1;
    let want_last = end - 1;
    let mut emitted_first = false;

    for row in &rows {
        let chunk_first = row.line_start as u64;
        let chunk_last = chunk_first + row.line_count as u64 - 1;
        if chunk_last < want_first || chunk_first > want_last {
            continue;
        }

        let text = chunk_lines_text(state.log_storage.as_ref(), log_key, row).await?;
        let lines: Vec<&str> = text.split_inclusive('\n').collect();

        let lo = usize::try_from(want_first.saturating_sub(chunk_first)).unwrap_or(usize::MAX);
        let hi = usize::try_from(want_last.min(chunk_last) - chunk_first).unwrap_or(usize::MAX);
        let hi = hi.min(lines.len().saturating_sub(1));

        if !emitted_first {
            let mut st = SgrState::default();
            st.apply_text(&row.color_prefix);
            for line in &lines[..lo.min(lines.len())] {
                st.apply_text(line);
            }
            out.push_str(&st.to_prefix());
            emitted_first = true;
        }

        for line in &lines[lo.min(lines.len())..=hi] {
            out.push_str(line);
        }
    }

    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], out).into_response())
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LogSearchQuery {
    pub q: String,
    #[serde(default)]
    pub case: bool,
}

#[utoipa::path(
    get,
    path = "/builds/{build}/log/search",
    operation_id = "searchBuildLog",
    tag = "builds",
    summary = "Search a build log",
    description = "Streams NDJSON search hits in scan order as the server decompresses chunks,\nso first matches and a running count appear immediately. Each line is a\n`LogSearchHit`; the final frame is a `LogSearchDone` carrying `total_matches`.\nMatches are per-line substrings (no multi-line matches).\n\n**Access:** Same as `GET /builds/{build}/log`.",
    params(("build" = BuildJobId, Path), LogSearchQuery),
    responses((status = OK, description = "NDJSON stream of search hits, terminated by a done frame", body = LogSearchHit, content_type = "application/x-ndjson"), WebError)
)]
pub async fn get_build_log_search(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
    Query(query): Query<LogSearchQuery>,
) -> Result<Response, WebError> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let (log_key, rows) = match super::effective_log_id(&state.web_db, &ctx.shared_build).await {
        Some(key) => (
            key,
            gradient_db::build_log_chunks::of_attempt(&state.web_db, key).await?,
        ),
        None => {
            return Ok(
                StreamBodyAs::json_nl(futures::stream::empty::<serde_json::Value>())
                    .into_response(),
            );
        }
    };

    let state = Arc::clone(&state);

    let case = query.case;
    let needle = if case {
        query.q.clone()
    } else {
        query.q.to_lowercase()
    };

    let stream = stream! {
        let mut total: u64 = 0;
        if !needle.is_empty() {
            for row in &rows {
                let raw = match state.log_storage.read_chunk(log_key, row.chunk_index as u32).await {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                let bytes = match zstd::stream::decode_all(&raw[..]) {
                    Ok(b) => b,
                    Err(_) => continue,
                };

                let text = String::from_utf8_lossy(&bytes);
                let mut byte_off = row.byte_start as u64;
                for (line_no, line) in (row.line_start as u64..).zip(text.split_inclusive('\n')) {
                    let haystack = if case { line.to_string() } else { line.to_lowercase() };
                    if haystack.contains(&needle) {
                        total += 1;
                        let hit = LogSearchHit {
                            line_number: line_no + 1,
                            chunk_index: row.chunk_index as u32,
                            byte_offset: byte_off,
                            preview: line.trim_end_matches('\n').chars().take(200).collect(),
                        };

                        yield serde_json::to_value(&hit).unwrap_or(serde_json::Value::Null);
                    }

                    byte_off += line.len() as u64;
                }
            }
        }

        let done = LogSearchDone { done: true, total_matches: total };
        yield serde_json::to_value(&done).unwrap_or(serde_json::Value::Null);
    };

    Ok(StreamBodyAs::json_nl(stream).into_response())
}

#[cfg(test)]
mod tests {
    use super::{ChunkRow, LineRangeQuery, chunk_lines_text, parse_line_range};
    use gradient_storage::{FileLogStorage, LogStorage};
    use gradient_types::BuildAttemptId;

    fn q(start: Option<u64>, end: Option<u64>, range: Option<&str>) -> LineRangeQuery {
        LineRangeQuery {
            start,
            end,
            range: range.map(|s| s.to_string()),
        }
    }

    #[test]
    fn parse_line_range_reads_start_end_and_range() {
        let cases = [
            (
                "start and end",
                q(Some(5), Some(9), None),
                Some((5, Some(9))),
            ),
            (
                "nothing given starts at line 1",
                q(None, None, None),
                Some((1, None)),
            ),
            (
                "start below 1 is raised to 1",
                q(Some(0), None, None),
                Some((1, None)),
            ),
            (
                "range with L prefixes",
                q(None, None, Some("L120-L130")),
                Some((120, Some(130))),
            ),
            ("bare range", q(None, None, Some("3-8")), Some((3, Some(8)))),
            (
                "range wins over start and end",
                q(Some(1), Some(2), Some("3-8")),
                Some((3, Some(8))),
            ),
            (
                "range without a dash",
                q(None, None, Some("nonsense")),
                None,
            ),
            (
                "range with a non-number",
                q(None, None, Some("L1-Lx")),
                None,
            ),
        ];

        for (case, query, expected) in cases {
            assert_eq!(parse_line_range(&query).ok(), expected, "{case}");
        }
    }

    fn row(chunk_index: i32, line_count: i32) -> ChunkRow {
        ChunkRow {
            chunk_index,
            line_count,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_missing_chunk_renders_one_placeholder_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let logs = FileLogStorage::new(dir.path()).await.unwrap();
        let key = BuildAttemptId::now_v7();
        let raw = zstd::stream::encode_all(&b"kept\n"[..], 1).unwrap();
        logs.write_chunk(key, 0, &raw).await.unwrap();

        assert_eq!(
            chunk_lines_text(&logs, key, &row(0, 1)).await.unwrap(),
            "kept\n"
        );
        assert_eq!(
            chunk_lines_text(&logs, key, &row(1, 3)).await.unwrap(),
            gradient_storage::log_chunk::MISSING_CHUNK_LINE.repeat(3)
        );
    }
}
