/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::BuildRequestRefusal;
use super::types::ManifestEntry;
use crate::access::{Caller, ProjectAccess, load_project};
use crate::authorization::MaybeApiKey;
use crate::error::WebResult;
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
use gradient_types::{
    ABuildRequestBlob, BaseResponse, CBuildRequestBlob, EBuildRequestBlob, MUploadSession, MUser,
    now,
};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct ManifestRequest {
    pub project: String,
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
    let project = load_project(
        &state.0,
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

    let max_source_upload_size = state.0.config.http.max_source_upload_size;
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

    let existing = EBuildRequestBlob::find()
        .filter(
            Condition::all()
                .add(CBuildRequestBlob::Project.eq(project.id))
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

    let manifest_value =
        serde_json::to_value(&body.files).context("Failed to serialise manifest")?;

    let missing_value =
        serde_json::to_value(&missing_hex).context("Failed to serialise missing list")?;

    let session_id = UploadSessionId::now_v7();
    let expires_at =
        now_ts + Duration::from_std(UPLOAD_SESSION_TTL).context("Invalid UPLOAD_SESSION_TTL")?;

    MUploadSession {
        id: session_id,
        project: project.id,
        manifest: manifest_value,
        missing: missing_value,
        total_size: total,
        created_at: now_ts,
        expires_at,
        ..Default::default()
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    Ok(ok_json(ManifestResponse {
        session: session_id,
        missing: missing_hex,
    }))
}
