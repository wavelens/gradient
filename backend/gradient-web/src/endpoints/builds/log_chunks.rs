/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use async_stream::stream;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use axum_streams::StreamBodyAs;
use gradient_storage::sgr::SgrState;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::Deserialize;
use std::sync::Arc;

use super::BuildAccessContext;

type ChunkRow = gradient_entity::build_log_chunk::Model;

async fn load_chunk_rows(state: &ServerState, log_key: BuildId) -> WebResult<Vec<ChunkRow>> {
    Ok(gradient_entity::build_log_chunk::Entity::find()
        .filter(gradient_entity::build_log_chunk::Column::Build.eq(log_key))
        .order_by_asc(gradient_entity::build_log_chunk::Column::ChunkIndex)
        .all(&state.web_db)
        .await?)
}

fn decode_chunk(raw: &[u8]) -> WebResult<String> {
    let bytes = zstd::stream::decode_all(raw)
        .map_err(|e| WebError::Internal(anyhow::Error::new(e)))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub async fn get_build_log_chunks(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildId>,
) -> WebResult<Json<BaseResponse<LogChunkIndex>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let log_key = super::effective_log_id(&state, &ctx.build).await;
    let rows = load_chunk_rows(&state, log_key).await?;

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
        total_chunks: chunks.len() as u32,
        total_lines,
        total_bytes,
        chunks,
    }))
}

pub async fn get_build_log_chunk(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((build_id, index)): Path<(BuildId, u32)>,
) -> Result<Response, WebError> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let log_key = super::effective_log_id(&state, &ctx.build).await;

    let row = gradient_entity::build_log_chunk::Entity::find()
        .filter(gradient_entity::build_log_chunk::Column::Build.eq(log_key))
        .filter(gradient_entity::build_log_chunk::Column::ChunkIndex.eq(index as i32))
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::not_found("LogChunk"))?;

    let raw = state
        .log_storage
        .read_chunk(log_key, index)
        .await
        .map_err(|_| WebError::not_found("LogChunk"))?;
    let text = format!("{}{}", row.color_prefix, decode_chunk(&raw)?);

    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
}

#[derive(Debug, Deserialize)]
pub struct LineRangeQuery {
    pub start: Option<u64>,
    pub end: Option<u64>,
    pub range: Option<String>,
}

/// Parse the requested 1-based inclusive line range from the query. Accepts
/// either `?start=&end=` or `?range=L120-L130` (the `L` prefixes are optional).
fn parse_line_range(q: &LineRangeQuery) -> Result<(u64, Option<u64>), WebError> {
    if let Some(range) = &q.range {
        let cleaned = range.replace(['L', 'l'], "");
        let (lo, hi) = cleaned
            .split_once('-')
            .ok_or_else(|| WebError::bad_request("range must look like L120-L130"))?;
        let start: u64 = lo
            .trim()
            .parse()
            .map_err(|_| WebError::bad_request("invalid range start"))?;
        let end: u64 = hi
            .trim()
            .parse()
            .map_err(|_| WebError::bad_request("invalid range end"))?;
        return Ok((start.max(1), Some(end)));
    }
    Ok((q.start.unwrap_or(1).max(1), q.end))
}

pub async fn get_build_log_lines(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildId>,
    Query(q): Query<LineRangeQuery>,
) -> Result<Response, WebError> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let log_key = super::effective_log_id(&state, &ctx.build).await;
    let rows = load_chunk_rows(&state, log_key).await?;

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
        let raw = state
            .log_storage
            .read_chunk(log_key, row.chunk_index as u32)
            .await
            .map_err(|_| WebError::not_found("LogChunk"))?;
        let text = decode_chunk(&raw)?;
        let lines: Vec<&str> = text.split_inclusive('\n').collect();

        let lo = want_first.saturating_sub(chunk_first) as usize;
        let hi = (want_last.min(chunk_last) - chunk_first) as usize;
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

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub case: bool,
}

pub async fn get_build_log_search(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildId>,
    Query(query): Query<SearchQuery>,
) -> Result<Response, WebError> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let log_key = super::effective_log_id(&state, &ctx.build).await;
    let rows = load_chunk_rows(&state, log_key).await?;
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
    use super::{parse_line_range, LineRangeQuery};

    fn q(start: Option<u64>, end: Option<u64>, range: Option<&str>) -> LineRangeQuery {
        LineRangeQuery {
            start,
            end,
            range: range.map(|s| s.to_string()),
        }
    }

    #[test]
    fn parses_start_end() {
        assert_eq!(parse_line_range(&q(Some(5), Some(9), None)).unwrap(), (5, Some(9)));
    }

    #[test]
    fn defaults_start_to_one() {
        assert_eq!(parse_line_range(&q(None, None, None)).unwrap(), (1, None));
    }

    #[test]
    fn parses_l_range() {
        assert_eq!(
            parse_line_range(&q(None, None, Some("L120-L130"))).unwrap(),
            (120, Some(130))
        );
    }

    #[test]
    fn parses_bare_range() {
        assert_eq!(
            parse_line_range(&q(None, None, Some("3-8"))).unwrap(),
            (3, Some(8))
        );
    }

    #[test]
    fn rejects_malformed_range() {
        assert!(parse_line_range(&q(None, None, Some("nonsense"))).is_err());
    }
}
