/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /build-requests/manifest` — first step of the build-request upload
//! flow. The client submits the full `(path, hash, size)` list; the server
//! validates paths/sizes, looks up which blobs the org already has, persists
//! an `upload_session` row, and returns the missing-hash set so the client
//! knows exactly what to upload next.

use crate::access::{Caller, OrgAccess, load_org};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::extract::{Json, State};
use axum::Extension;
use chrono::Duration;
use gradient_core::constants::{MAX_BUILD_REQUEST_SIZE, UPLOAD_SESSION_TTL};
use gradient_core::types::ids::UploadSessionId;
use gradient_core::types::{
    ABuildRequestBlob, AUploadSession, BaseResponse, CBuildRequestBlob, EBuildRequestBlob, MUser,
    ServerState, now,
};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ManifestEntry {
    pub path: String,
    pub hash: String,
    pub size: i64,
}

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

    let manifest_value = serde_json::to_value(&body.files).map_err(|e| {
        WebError::internal(format!("Failed to serialise manifest: {}", e))
    })?;
    let missing_value = serde_json::to_value(&missing_hex).map_err(|e| {
        WebError::internal(format!("Failed to serialise missing list: {}", e))
    })?;

    let session_id = UploadSessionId::now_v7();
    let expires_at = now_ts
        + Duration::from_std(UPLOAD_SESSION_TTL).map_err(|e| {
            WebError::internal(format!("Invalid UPLOAD_SESSION_TTL: {}", e))
        })?;

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

fn validate_manifest_path(path: &str) -> WebResult<()> {
    if path.is_empty() {
        return Err(WebError::bad_request("Empty path in manifest"));
    }
    if path.contains('\0') {
        return Err(WebError::bad_request(format!(
            "Invalid path (null byte): {}",
            path
        )));
    }
    let p = Path::new(path);
    if p.is_absolute() {
        return Err(WebError::bad_request(format!("Absolute path: {}", path)));
    }
    for component in p.components() {
        match component {
            Component::Normal(_) => {}
            _ => {
                return Err(WebError::bad_request(format!(
                    "Invalid path component in: {}",
                    path
                )));
            }
        }
    }
    Ok(())
}

fn decode_blake3_hex(hash: &str) -> WebResult<Vec<u8>> {
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)) {
        return Err(WebError::bad_request(format!(
            "Hash must be 64-char lowercase hex: {}",
            hash
        )));
    }
    hex::decode(hash).map_err(|e| WebError::bad_request(format!("Invalid hash hex: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal() {
        assert!(validate_manifest_path("../escape").is_err());
        assert!(validate_manifest_path("foo/../bar").is_err());
        assert!(validate_manifest_path("a/./b").is_err());
    }

    #[test]
    fn rejects_absolute_and_empty_paths() {
        assert!(validate_manifest_path("/absolute").is_err());
        assert!(validate_manifest_path("").is_err());
        assert!(validate_manifest_path(".").is_err());
    }

    #[test]
    fn rejects_null_bytes() {
        assert!(validate_manifest_path("with\0null").is_err());
    }

    #[test]
    fn accepts_normal_nested_paths() {
        assert!(validate_manifest_path("flake.nix").is_ok());
        assert!(validate_manifest_path("src/main.rs").is_ok());
        assert!(validate_manifest_path("a/b/c/d.txt").is_ok());
    }

    #[test]
    fn hash_must_be_64_lowercase_hex() {
        let ok = "a".repeat(64);
        assert!(decode_blake3_hex(&ok).is_ok());
        let too_short = "a".repeat(63);
        assert!(decode_blake3_hex(&too_short).is_err());
        let uppercase = "A".repeat(64);
        assert!(decode_blake3_hex(&uppercase).is_err());
        let non_hex = "g".repeat(64);
        assert!(decode_blake3_hex(&non_hex).is_err());
    }
}
