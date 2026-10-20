/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::BuildRequestRefusal;
use super::types::ManifestEntry;
use crate::access::{Caller, ProjectAccess, load_project};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use anyhow::Context;
use axum::Extension;
use axum::extract::{Json, State};
use chrono::Duration;
use gradient_core::ServerState;
use gradient_types::constants::UPLOAD_SESSION_TTL;
use gradient_types::ids::UploadSessionId;
use gradient_types::input::{check_manifest_path, decode_blake3_hex};
use gradient_types::{BaseResponse, MUploadSession, MUser, now};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ManifestRequest {
    pub project: String,
    pub files: Vec<ManifestEntry>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ManifestResponse {
    pub session: UploadSessionId,
    pub missing: Vec<String>,
}

#[utoipa::path(
    post,
    path = "/build-requests/manifest",
    operation_id = "postBuildRequestManifest",
    tag = "build-requests",
    summary = "Submit a build-request file manifest",
    description = "Step 1 of the `gradient build` flow. Posts the list of git-tracked files\nin the working tree, each addressed by BLAKE3 content hash. The server\nreturns an `UploadSession` plus the subset of hashes it does not yet\nhave on disk; the client uploads those via `POST\n/build-requests/{session}/blobs` and then finalises with `POST\n/build-requests/{session}/dispatch`.\n\nThe combined `size` of all files must not exceed `MAX_BUILD_REQUEST_SIZE`\n(20 MiB). Paths must be relative, free of `.`/`..` components, free of\nnull bytes, and unique within the manifest.",
    request_body = ManifestRequest,
    responses((status = OK, description = "Session created", body = ManifestResponse), WebError)
)]
pub async fn post_manifest(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Json(body): Json<ManifestRequest>,
) -> WebResult<Json<BaseResponse<ManifestResponse>>> {
    let project = load_project(
        &state.0.db(),
        Caller::User(&user),
        api_key.as_ref(),
        body.project.clone(),
        ProjectAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    if body.files.is_empty() {
        return Err(BuildRequestRefusal::ManifestEmpty.into());
    }

    let max_source_upload_size = state.0.config.http.max_source_upload_size.get();
    let mut total: i64 = 0;
    let mut seen_paths: HashSet<&str> = HashSet::with_capacity(body.files.len());
    let mut hashes: Vec<Vec<u8>> = Vec::with_capacity(body.files.len());

    for entry in &body.files {
        check_manifest_path(&entry.path)?;
        if !seen_paths.insert(entry.path.as_str()) {
            return Err(BuildRequestRefusal::DuplicateManifestPath {
                path: entry.path.clone(),
            }
            .into());
        }

        if entry.size < 0 {
            return Err(BuildRequestRefusal::NegativeFileSize {
                path: entry.path.clone(),
            }
            .into());
        }

        total = total
            .checked_add(entry.size)
            .ok_or(BuildRequestRefusal::ManifestSizeOverflow)?;

        if (total as u128) > max_source_upload_size as u128 {
            return Err(BuildRequestRefusal::ManifestTooLarge {
                limit: max_source_upload_size,
            }
            .into());
        }

        hashes.push(decode_blake3_hex(&entry.hash)?);
    }

    let unique_hashes: Vec<Vec<u8>> = {
        let mut seen = HashSet::new();
        hashes
            .iter()
            .filter(|h| seen.insert((*h).clone()))
            .cloned()
            .collect()
    };

    let now_ts = now();
    let existing_set: HashSet<Vec<u8>> = gradient_db::build_request_blobs::mark_stored_as_used(
        &state.web_db,
        project.id,
        unique_hashes.clone(),
        now_ts,
    )
    .await?
    .into_iter()
    .collect();

    let missing_hex: Vec<String> = unique_hashes
        .iter()
        .filter(|h| !existing_set.contains(*h))
        .map(hex::encode)
        .collect();

    let manifest_value = serde_json::to_value(&body.files).context("serialise manifest")?;

    let missing_value = serde_json::to_value(&missing_hex).context("serialise missing list")?;

    let session_id = UploadSessionId::now_v7();
    let expires_at =
        now_ts + Duration::from_std(UPLOAD_SESSION_TTL).context("invalid UPLOAD_SESSION_TTL")?;

    gradient_db::upload_sessions::insert(
        &state.web_db,
        MUploadSession {
            id: session_id,
            project: project.id,
            manifest: manifest_value,
            missing: missing_value,
            total_size: total,
            created_at: now_ts,
            expires_at,
            ..Default::default()
        },
    )
    .await?;

    Ok(ok_json(ManifestResponse {
        session: session_id,
        missing: missing_hex,
    }))
}
