/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::dispatch::{
    BuildRequestOptions, DispatchResponse, InputOverrideBody, finalize_build_request, parent_job,
};
use super::refusal::BuildRequestRefusal;
use crate::access::{Caller, ProjectAccess, load_project};
use crate::api_spec::Binary;
use crate::authorization::{MaybeApiKey, Principal};
use crate::endpoints::ChunkReceived;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use anyhow::Context;
use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Multipart, Path, Query, State};
use gradient_core::ServerState;
use gradient_storage::PartialStore;
use gradient_storage::source_nar::source_nar_from_bytes;
use gradient_types::RuntimeConfig;
use gradient_types::{BaseResponse, MUser};
use serde::Deserialize;
use std::sync::Arc;

#[derive(utoipa::ToSchema)]
pub struct SourceUploadForm {
    #[schema(value_type = String, format = Binary)]
    pub nar: Vec<u8>,
    pub target: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SourceQuery {
    pub project: String,
}

#[utoipa::path(
    post,
    path = "/build-requests/source",
    operation_id = "postBuildRequestSource",
    tag = "build-requests",
    summary = "Upload a pre-packed source NAR and queue an evaluation (single-shot)",
    description = "One-shot alternative to the manifest/blob flow for clients built with\nthe `nix` feature. The client NAR-packs the git-tracked source itself\nand uploads it here; the server computes the `/nix/store/<hash>-source`\npath, signs it, lazily creates the per-project `build-request` task, and\nqueues an evaluation. The whole NAR is one request, bounded by\n`max_source_upload_size`; large sources should use the chunked\n`POST /build-requests/source/{upload}/chunk` + `finalize` flow instead.",
    params(SourceQuery),
    request_body(content = SourceUploadForm, content_type = "multipart/form-data"),
    responses((status = OK, description = "Evaluation queued", body = DispatchResponse), WebError)
)]
pub async fn post_source(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    principal: Option<Extension<Principal>>,
    Query(query): Query<SourceQuery>,
    mut multipart: Multipart,
) -> WebResult<Json<BaseResponse<DispatchResponse>>> {
    let project = load_project(
        &state.0.db(),
        Caller::User(&user),
        api_key.as_ref(),
        query.project,
        ProjectAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    let mut nar_bytes: Option<Vec<u8>> = None;
    let mut target: Option<String> = None;

    while let Some(field) =
        multipart
            .next_field()
            .await
            .map_err(|e| BuildRequestRefusal::InvalidMultipartPayload {
                reason: e.to_string(),
            })?
    {
        match field.name() {
            Some("nar") => {
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| BuildRequestRefusal::UnreadableNar {
                        reason: e.to_string(),
                    })?;
                nar_bytes = Some(data.to_vec());
            }
            Some("target") => target = field.text().await.ok(),
            _ => {}
        }
    }

    let nar_bytes = nar_bytes.ok_or(BuildRequestRefusal::MissingNarField)?;
    if nar_bytes.is_empty() {
        return Err(BuildRequestRefusal::EmptyNarField.into());
    }

    let nar = source_nar_from_bytes(nar_bytes)
        .await
        .context("read source NAR")?;

    let response = finalize_build_request(
        &state.db(),
        &state.graph,
        project.id,
        &user,
        &nar,
        BuildRequestOptions {
            target,
            input_overrides: Vec::new(),
            session: None,
            parent_job: parent_job(principal),
        },
    )
    .await?;

    Ok(ok_json(response))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SourceChunkQuery {
    offset: u64,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SourceFinalize {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub input_overrides: Vec<InputOverrideBody>,
}

fn source_partial_store(config: &RuntimeConfig) -> WebResult<PartialStore> {
    Ok(PartialStore::new(
        config.server.source_upload_partial_dir(),
    )?)
}

fn require_safe_upload_id(upload: &str) -> WebResult<()> {
    if upload.is_empty() || upload.contains('/') || upload.contains("..") {
        return Err(BuildRequestRefusal::InvalidUploadId.into());
    }
    Ok(())
}

#[utoipa::path(
    put,
    path = "/build-requests/source/{upload}/chunk",
    operation_id = "putBuildRequestSourceChunk",
    tag = "build-requests",
    summary = "Append a chunk of a source NAR to a staged upload",
    description = "Streams a large source NAR to the server in slices so no single request\nhits a body limit. `upload` is a client-chosen id (the CLI uses the NAR's\ncontent hash, which makes a retry resumable). `offset` must equal the\nbytes already received (`0` starts fresh); a mismatch appends nothing and\nreturns the authoritative `received` so the client resumes. The staged\ntotal is bounded by `max_source_upload_size`. Finalise with\n`POST /build-requests/source/{upload}/finalize`.",
    params(("upload" = String, Path), SourceChunkQuery),
    request_body(content = Binary, content_type = "application/octet-stream"),
    responses((status = OK, description = "Chunk staged; returns total bytes received so far.", body = ChunkReceived), WebError)
)]
pub async fn source_chunk(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(upload): Path<String>,
    Query(SourceChunkQuery { offset }): Query<SourceChunkQuery>,
    body: Bytes,
) -> WebResult<Json<BaseResponse<ChunkReceived>>> {
    require_safe_upload_id(&upload)?;

    let max = u64::try_from(state.config.http.max_source_upload_size.get()).unwrap_or(u64::MAX);
    if offset + body.len() as u64 > max {
        return Err(BuildRequestRefusal::SourceUploadTooLarge { max }.into());
    }

    let store = source_partial_store(&state.config)?;
    let key = format!("{}/{upload}", user.id);
    let staged = store.received_len(&key, &upload).await?;
    let received = if offset == 0 || offset == staged {
        store.append(&key, &upload, offset, &body).await?;
        offset + body.len() as u64
    } else {
        staged
    };

    Ok(ok_json(ChunkReceived { received }))
}

#[utoipa::path(
    post,
    path = "/build-requests/source/{upload}/finalize",
    operation_id = "postBuildRequestSourceFinalize",
    tag = "build-requests",
    summary = "Finalise a chunked source upload and queue an evaluation",
    description = "Reassembles the staged source NAR for `upload`, computes its\n`/nix/store/<hash>-source` path, signs it, lazily creates the per-project\n`build-request` task, queues an evaluation, and drops the partial.",
    params(("upload" = String, Path), SourceQuery),
    request_body = SourceFinalize,
    responses((status = OK, description = "Evaluation queued", body = DispatchResponse), WebError)
)]
pub async fn source_finalize(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    principal: Option<Extension<Principal>>,
    Path(upload): Path<String>,
    Query(query): Query<SourceQuery>,
    Json(body): Json<SourceFinalize>,
) -> WebResult<Json<BaseResponse<DispatchResponse>>> {
    require_safe_upload_id(&upload)?;

    let project = load_project(
        &state.0.db(),
        Caller::User(&user),
        api_key.as_ref(),
        query.project,
        ProjectAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    let store = source_partial_store(&state.config)?;
    let key = format!("{}/{upload}", user.id);
    let nar_bytes =
        store
            .read_all(&key)
            .await
            .map_err(|e| BuildRequestRefusal::NoStagedSourceUpload {
                reason: e.to_string(),
            })?;

    if nar_bytes.is_empty() {
        let _ = store.discard(&key).await;
        return Err(BuildRequestRefusal::EmptySourceUpload.into());
    }

    let nar = source_nar_from_bytes(nar_bytes)
        .await
        .context("read source NAR")?;

    let input_overrides = body
        .input_overrides
        .into_iter()
        .map(|o| (o.input_name, o.url))
        .collect();

    let response = finalize_build_request(
        &state.db(),
        &state.graph,
        project.id,
        &user,
        &nar,
        BuildRequestOptions {
            target: body.target,
            input_overrides,
            session: None,
            parent_job: parent_job(principal),
        },
    )
    .await?;

    let _ = store.discard(&key).await;

    Ok(ok_json(response))
}

#[cfg(test)]
mod tests {
    use super::require_safe_upload_id;

    #[test]
    fn upload_id_rejects_path_traversal() {
        assert!(require_safe_upload_id("a1b2c3deadbeef").is_ok());
        assert!(require_safe_upload_id("").is_err());
        assert!(require_safe_upload_id("a/b").is_err());
        assert!(require_safe_upload_id("..").is_err());
        assert!(require_safe_upload_id("../../etc/passwd").is_err());
    }
}
