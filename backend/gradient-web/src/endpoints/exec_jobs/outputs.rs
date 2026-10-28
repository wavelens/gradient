/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::api_spec::Binary;
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::untrusted_content_headers;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;
use anyhow::Context;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use futures::stream::{BoxStream, Stream, StreamExt};
use gradient_core::ServerState;
use gradient_db::exec_jobs::outputs;
use gradient_storage::NarStore;
use gradient_types::*;
use serde::Serialize;
use std::sync::Arc;

use super::ExecJobAccessContext;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ExecJobOutputView {
    pub path: String,
    pub size: i64,
    pub content_type: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl From<MExecJobOutput> for ExecJobOutputView {
    fn from(row: MExecJobOutput) -> Self {
        Self {
            path: row.path,
            size: row.size,
            content_type: row.content_type,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ByteRange {
    Whole,
    Part { start: u64, end: u64 },
    Unsatisfiable,
}

pub(crate) fn parse_byte_range(header: &str, size: u64) -> ByteRange {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return ByteRange::Whole;
    };
    if spec.contains(',') {
        return ByteRange::Whole;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return ByteRange::Whole;
    };
    match (first.trim(), last.trim()) {
        ("", suffix) => suffix_range(suffix, size),
        (first, last) => bounded_range(first, last, size),
    }
}

fn suffix_range(suffix: &str, size: u64) -> ByteRange {
    match suffix.parse::<u64>() {
        Ok(0) => ByteRange::Unsatisfiable,
        Ok(_) if size == 0 => ByteRange::Unsatisfiable,
        Ok(length) => ByteRange::Part {
            start: size.saturating_sub(length),
            end: size - 1,
        },
        Err(_) => ByteRange::Whole,
    }
}

fn bounded_range(first: &str, last: &str, size: u64) -> ByteRange {
    let Ok(start) = first.parse::<u64>() else {
        return ByteRange::Whole;
    };
    let end = match last {
        "" => None,
        last => match last.parse::<u64>() {
            Ok(end) if end >= start => Some(end),
            _ => return ByteRange::Whole,
        },
    };
    if start >= size {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Part {
        start,
        end: end.map_or(size - 1, |end| end.min(size - 1)),
    }
}

fn limited_to(
    stream: BoxStream<'static, anyhow::Result<Bytes>>,
    length: u64,
) -> impl Stream<Item = anyhow::Result<Bytes>> + Send + 'static {
    stream.scan(length, |left, chunk| {
        if *left == 0 {
            return futures::future::ready(None);
        }
        let chunk = chunk.map(|mut bytes| {
            let keep = usize::try_from(*left).map_or(bytes.len(), |left| left.min(bytes.len()));
            bytes.truncate(keep);
            *left -= keep as u64;
            bytes
        });
        futures::future::ready(Some(chunk))
    })
}

fn attachment_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .chars()
        .map(|c| {
            if c == '"' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn unsatisfiable(size: u64) -> WebResult<Response> {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::CONTENT_RANGE, format!("bytes */{size}"))
        .body(Body::empty())
        .context("build response")
        .map_err(Into::into)
}

async fn output_body(
    store: &NarStore,
    job: ExecJobId,
    path: &str,
    start: u64,
    length: u64,
) -> WebResult<Body> {
    let (_, stream) = store
        .open_exec_output(job.into_inner(), path, start)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::ExecOutput))?;
    Ok(Body::from_stream(limited_to(stream, length)))
}

#[utoipa::path(
    get,
    path = "/exec-jobs/{job}/outputs",
    operation_id = "listExecJobOutputs",
    tag = "exec-jobs",
    summary = "List exec job outputs",
    description = "Returns every output the job uploaded, ordered by path, with its current size. An output written again by a later upload shows its new size and `updated_at`.\n\n**Access:** Public projects, or members of the project.",
    params(("job" = ExecJobId, Path)),
    responses((status = OK, description = "Exec job outputs", body = Vec<ExecJobOutputView>), WebError)
)]
pub async fn list_exec_job_outputs(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(job): Path<ExecJobId>,
) -> WebResult<Json<BaseResponse<Vec<ExecJobOutputView>>>> {
    ExecJobAccessContext::load(&state.web_db, job, &maybe_user, api_key.as_ref()).await?;
    let rows = outputs::of_job(&state.web_db, job).await?;
    Ok(ok_json(rows.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    get,
    path = "/exec-jobs/{job}/outputs/{path}",
    operation_id = "getExecJobOutput",
    tag = "exec-jobs",
    summary = "Download an exec job output",
    description = "Streams one output as an attachment. A single `Range: bytes=<start>-[<end>]` answers 206 with that part, a start at or past the size answers 416 and several ranges answer the whole output.\n\n**Access:** Public projects, or members of the project.",
    params(("job" = ExecJobId, Path), ("path" = String, Path)),
    responses(
        (status = OK, description = "Output contents", body = Binary, content_type = "application/octet-stream"),
        (status = PARTIAL_CONTENT, description = "Requested part of the output", body = Binary, content_type = "application/octet-stream"),
        (status = RANGE_NOT_SATISFIABLE, description = "The range starts at or past the size"),
        WebError
    )
)]
pub async fn get_exec_job_output(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((job, path)): Path<(ExecJobId, String)>,
    headers: HeaderMap,
) -> WebResult<Response> {
    ExecJobAccessContext::load(&state.web_db, job, &maybe_user, api_key.as_ref()).await?;
    let row = outputs::one(&state.web_db, job, &path)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::ExecOutput))?;
    let size = u64::try_from(row.size).unwrap_or(0);
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map_or(ByteRange::Whole, |value| parse_byte_range(value, size));

    let (status, start, length) = match range {
        ByteRange::Unsatisfiable => return unsatisfiable(size),
        ByteRange::Whole => (StatusCode::OK, 0, size),
        ByteRange::Part { start, end } => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
    };

    let mut response = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, length)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&row.content_type)
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        )
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", attachment_name(&path)),
        );
    for (name, value) in untrusted_content_headers() {
        response = response.header(name, value);
    }
    if let ByteRange::Part { start, end } = range {
        response = response.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }

    let body = output_body(&state.nar_storage, job, &path, start, length).await?;
    response
        .body(body)
        .context("build response")
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_byte_range_is_served_and_one_past_the_end_is_unsatisfiable() {
        assert_eq!(
            parse_byte_range("bytes=2-", 6),
            ByteRange::Part { start: 2, end: 5 }
        );
        assert_eq!(
            parse_byte_range("bytes=0-99", 6),
            ByteRange::Part { start: 0, end: 5 }
        );
        assert_eq!(
            parse_byte_range("bytes=-2", 6),
            ByteRange::Part { start: 4, end: 5 }
        );
        assert_eq!(parse_byte_range("bytes=6-", 6), ByteRange::Unsatisfiable);
        assert_eq!(parse_byte_range("bytes=0-1,3-4", 6), ByteRange::Whole);
    }
}
