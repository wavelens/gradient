/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, cache_client_ip, fetch_nar_stream};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use gradient_core::ServerState;
use gradient_storage::nar_extract::{
    ExtractError, Extracted, extract_path_from_reader, nar_reader_from_stream,
};
use std::sync::Arc;

pub async fn serve(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, hash, rel_path)): Path<(String, String, String)>,
) -> WebResult<Response> {
    let client_ip = cache_client_ip(&state, &headers, peer);
    let _ctx = CacheContext::load(&state, &headers, client_ip, cache).await?;
    let (_effective_hash, _size, stream) = fetch_nar_stream(&state, &hash).await?;
    let reader = nar_reader_from_stream(stream);

    match extract_path_from_reader(reader, &rel_path).await {
        Ok(Extracted::File { contents, size, .. }) => {
            let ct = mime_guess::from_path(&rel_path).first_or_octet_stream();
            Response::builder()
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(ct.as_ref())
                        .unwrap_or(HeaderValue::from_static("application/octet-stream")),
                )
                .header(header::CONTENT_LENGTH, size)
                .body(Body::from(contents))
                .map_err(|e| WebError::internal(format!("Failed to build response: {}", e)))
        }
        Ok(Extracted::Directory { tar_zst }) => {
            let basename = rel_path
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or("dir");
            let disp = format!("attachment; filename=\"{}.tar.zst\"", basename);
            Response::builder()
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/zstd"),
                )
                .header(
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_str(&disp).unwrap_or(HeaderValue::from_static("attachment")),
                )
                .body(Body::from(tar_zst))
                .map_err(|e| WebError::internal(format!("Failed to build response: {}", e)))
        }
        Err(ExtractError::NotFound) => Err(WebError::not_found("Path")),
        Err(ExtractError::Io(e)) => {
            Err(WebError::internal(format!("NAR extraction failed: {}", e)))
        }
    }
}
