/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /build-requests/manifest` - first step of the build-request upload
//! flow. The client submits the full `(path, hash, size)` list; the server
//! validates paths/sizes, looks up which blobs the org already has, persists
//! an `upload_session` row, and returns the missing-hash set so the client
//! knows exactly what to upload next.

use super::types::ManifestEntry;
use super::validation::{decode_blake3_hex, validate_manifest_path};
use crate::access::{Caller, OrgAccess, load_org};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::Extension;
use axum::extract::{Json, State};
use chrono::Duration;
use gradient_types::constants::{MAX_BUILD_REQUEST_SIZE, UPLOAD_SESSION_TTL};
use gradient_types::ids::UploadSessionId;
use gradient_types::{
    ABuildRequestBlob, AUploadSession, BaseResponse, CBuildRequestBlob, EBuildRequestBlob, MUser,
    now,
};
use gradient_core::ServerState;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct ManifestRequest {
    pub organization: String,
    pub files: Vec<ManifestEntry>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ManifestResponse {
    pub session: UploadSessionId,
    pub missing: Vec<String>,
}

pub async fn post_manifest(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Json(body): Json<ManifestRequest>,
) -> WebResult<Json<BaseResponse<ManifestResponse>>> {
    let org = load_org(
        &state.0,
        Caller::User(&user),
        api_key.as_ref(),
        body.organization.clone(),
        OrgAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    if body.files.is_empty() {
        return Err(WebError::bad_request(
            "Manifest must contain at least one file",
        ));
    }

    let mut total: i64 = 0;
    let mut seen_paths: HashSet<&str> = HashSet::with_capacity(body.files.len());
    let mut hashes: Vec<Vec<u8>> = Vec::with_capacity(body.files.len());

    for entry in &body.files {
        validate_manifest_path(&entry.path)?;
        if !seen_paths.insert(entry.path.as_str()) {
            return Err(WebError::bad_request(format!(
                "Duplicate path in manifest: {}",
                entry.path
            )));
        }
        if entry.size < 0 {
            return Err(WebError::bad_request(format!(
                "Negative size for {}",
                entry.path
            )));
        }
        total = total.checked_add(entry.size).ok_or_else(|| {
            WebError::payload_too_large("Manifest total size overflows i64".to_string())
        })?;
        if (total as u128) > MAX_BUILD_REQUEST_SIZE as u128 {
            return Err(WebError::payload_too_large(format!(
                "Manifest total size exceeds limit of {} bytes",
                MAX_BUILD_REQUEST_SIZE
            )));
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

    let existing = EBuildRequestBlob::find()
        .filter(
            Condition::all()
                .add(CBuildRequestBlob::Organization.eq(org.id))
                .add(CBuildRequestBlob::Hash.is_in(unique_hashes.clone())),
        )
        .all(&state.web_db)
        .await?;

    let existing_set: HashSet<Vec<u8>> = existing.iter().map(|b| b.hash.clone()).collect();

    let now_ts = now();
    for blob in &existing {
        let mut active: ABuildRequestBlob = blob.clone().into();
        active.last_used_at = Set(now_ts);
        active.update(&state.web_db).await?;
    }

    let missing_hex: Vec<String> = unique_hashes
        .iter()
        .filter(|h| !existing_set.contains(*h))
        .map(hex::encode)
        .collect();

    let manifest_value = serde_json::to_value(&body.files)
        .map_err(|e| WebError::internal(format!("Failed to serialise manifest: {}", e)))?;
    let missing_value = serde_json::to_value(&missing_hex)
        .map_err(|e| WebError::internal(format!("Failed to serialise missing list: {}", e)))?;

    let session_id = UploadSessionId::now_v7();
    let expires_at = now_ts
        + Duration::from_std(UPLOAD_SESSION_TTL)
            .map_err(|e| WebError::internal(format!("Invalid UPLOAD_SESSION_TTL: {}", e)))?;

    AUploadSession {
        id: Set(session_id),
        organization: Set(org.id),
        manifest: Set(manifest_value),
        missing: Set(missing_value),
        total_size: Set(total),
        created_at: Set(now_ts),
        expires_at: Set(expires_at),
        dispatched_at: Set(None),
    }
    .insert(&state.web_db)
    .await?;

    Ok(ok_json(ManifestResponse {
        session: session_id,
        missing: missing_hex,
    }))
}
