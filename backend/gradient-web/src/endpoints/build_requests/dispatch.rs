/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /build-requests/{session}/dispatch` - finalises a build-request
//! upload session by materialising the staged blobs into a
//! `/nix/store/<hash>-source` path, persisting `cached_path` metadata,
//! lazily creating a per-org `build-request` project, and queueing an
//! evaluation for the scheduler to pick up.

use super::types::ManifestEntry;
use super::validation::{decode_blake3_hex, validate_manifest_path};
use crate::access::has_permission;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use gradient_core::ServerState;
use gradient_storage::source_nar::{SourceNar, materialise_source_nar};
use gradient_types::ConcurrencyPolicy;
use gradient_types::ids::{
    CachedPathId, CachedPathSignatureId, CommitId, EvaluationId, ProjectId, UploadSessionId,
};
use gradient_types::{
    ACachedPathSignature, AUploadSession, BaseResponse, CCachedPath, CCachedPathSignature,
    COrganizationCache, CProject, ECache, ECachedPath, ECachedPathSignature, EOrganizationCache,
    EProject, EUploadSession, MCachedPath, MCachedPathSignature, MCommit, MEvaluation, MProject,
    MUser, NULL_TIME, now,
};
use gradient_util::nix_hash::normalize_nar_hash;
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, RuntimeErr, TransactionTrait, sqlx,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::fs;

const BUILD_REQUEST_PROJECT_NAME: &str = "build-request";

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct DispatchRequest {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DispatchResponse {
    pub evaluation: EvaluationId,
    pub project: ProjectId,
    pub commit: CommitId,
    pub cache: Option<String>,
}

pub async fn post_dispatch(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(session_id): Path<UploadSessionId>,
    Json(body): Json<DispatchRequest>,
) -> WebResult<Json<BaseResponse<DispatchResponse>>> {
    let session = EUploadSession::find_by_id(session_id)
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::not_found("Upload session"))?;

    if !has_permission(
        &state,
        user.id,
        session.organization,
        Permission::TriggerEvaluation,
        api_key.as_ref(),
    )
    .await?
    {
        return Err(WebError::not_found("Upload session"));
    }

    if session.dispatched_at.is_some() {
        return Err(WebError::conflict("Upload session already dispatched"));
    }
    if now() > session.expires_at {
        return Err(WebError::gone("Upload session expired"));
    }

    let missing: Vec<String> = serde_json::from_value(session.missing.clone())
        .map_err(|e| WebError::internal(format!("Corrupt session.missing JSON: {}", e)))?;
    if !missing.is_empty() {
        return Err(WebError::conflict(format!(
            "{} blobs still missing",
            missing.len()
        )));
    }

    let manifest: Vec<ManifestEntry> = serde_json::from_value(session.manifest.clone())
        .map_err(|e| WebError::internal(format!("Corrupt session.manifest JSON: {}", e)))?;

    let staging = TempDir::new()
        .map_err(|e| WebError::internal(format!("Failed to create staging dir: {}", e)))?;
    materialise_staging(
        &state,
        &session.organization.into_inner(),
        &manifest,
        staging.path(),
    )
    .await?;

    let nar = materialise_source_nar(staging.path())
        .await
        .map_err(|e| WebError::internal(format!("Failed to materialise source NAR: {}", e)))?;

    let response = finalize_build_request(
        &state,
        session.organization,
        &user,
        &nar,
        body.target,
        body.system,
    )
    .await?;

    let mut active: AUploadSession = session.into();
    active.dispatched_at = Set(Some(now()));
    active.update(&state.web_db).await?;

    Ok(ok_json(response))
}

/// Materialise a source NAR into the cache and queue a build-request evaluation.
/// Shared by the blob-manifest dispatch and the `nix`-feature source-NAR upload.
pub(super) async fn finalize_build_request(
    state: &ServerState,
    organization: gradient_types::ids::OrganizationId,
    user: &MUser,
    nar: &SourceNar,
    target: Option<String>,
    system: Option<String>,
) -> WebResult<DispatchResponse> {
    let _ = system;

    state
        .nar_storage
        .put(&nar.store_hash, nar.compressed_bytes.clone())
        .await
        .map_err(|e| WebError::internal(format!("Failed to store source NAR: {}", e)))?;

    let tx = state.web_db.inner().begin().await?;

    let cached_path = ensure_cached_path(&tx, nar).await?;
    queue_signature_placeholders(&tx, &cached_path, &organization).await?;
    let project = ensure_build_request_project(&tx, organization, user.id).await?;

    let target = target
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| project.wildcard.clone());

    let commit = MCommit {
        id: CommitId::now_v7(),
        message: format!("Build request {}", nar.store_hash),
        hash: vec![0; 20],
        author: Some(user.id),
        author_name: user.name.clone(),
    }
    .into_active_model()
    .insert(&tx)
    .await?;

    let now_ts = now();
    let evaluation = MEvaluation {
        id: EvaluationId::now_v7(),
        project: Some(project.id),
        repository: nar.store_path.clone(),
        commit: commit.id,
        wildcard: target,
        status: gradient_entity::evaluation::EvaluationStatus::Queued,
        created_at: now_ts,
        updated_at: now_ts,
        ..Default::default()
    }
    .into_active_model()
    .insert(&tx)
    .await?;

    let cache = resolve_org_cache_name(&tx, organization).await?;

    tx.commit().await?;

    Ok(DispatchResponse {
        evaluation: evaluation.id,
        project: project.id,
        commit: commit.id,
        cache,
    })
}

async fn resolve_org_cache_name<C: ConnectionTrait>(
    tx: &C,
    org: gradient_types::ids::OrganizationId,
) -> WebResult<Option<String>> {
    let Some(link) = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(org))
        .one(tx)
        .await?
    else {
        return Ok(None);
    };

    Ok(ECache::find_by_id(link.cache)
        .one(tx)
        .await?
        .filter(|c| c.active)
        .map(|c| c.name))
}

async fn materialise_staging(
    state: &ServerState,
    org_uuid: &uuid::Uuid,
    manifest: &[ManifestEntry],
    root: &std::path::Path,
) -> WebResult<()> {
    for entry in manifest {
        validate_manifest_path(&entry.path)?;
        let hash_bytes = decode_blake3_hex(&entry.hash)?;
        let mut hash_array = [0u8; 32];
        hash_array.copy_from_slice(&hash_bytes);

        let data = state
            .nar_storage
            .get_blob(*org_uuid, &hash_array)
            .await
            .map_err(|e| WebError::internal(format!("Failed to fetch blob: {}", e)))?
            .ok_or_else(|| {
                WebError::internal(format!("Blob {} disappeared from storage", entry.hash))
            })?;

        let target = root.join(&entry.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| WebError::internal(format!("Failed to create dir: {}", e)))?;
        }
        fs::write(&target, data)
            .await
            .map_err(|e| WebError::internal(format!("Failed to write {}: {}", entry.path, e)))?;
    }
    Ok(())
}

async fn ensure_cached_path<C: ConnectionTrait>(
    tx: &C,
    nar: &SourceNar,
) -> WebResult<gradient_entity::cached_path::Model> {
    if let Some(existing) = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(nar.store_hash.clone()))
        .one(tx)
        .await?
    {
        return Ok(existing);
    }

    let nar_hash = normalize_nar_hash(&nar.nar_hash_sri);
    let file_hash = normalize_nar_hash(&nar.file_hash_sri);
    let row = MCachedPath {
        id: CachedPathId::now_v7(),
        hash: nar.store_hash.clone(),
        package: "source".to_string(),
        file_hash: Some(file_hash),
        file_size: Some(nar.file_size as i64),
        nar_size: Some(nar.nar_size as i64),
        nar_hash: Some(nar_hash),
        created_at: now(),
        ..Default::default()
    }
    .into_active_model();

    match row.insert(tx).await {
        Ok(model) => Ok(model),
        Err(err) if is_unique_violation(&err) => ECachedPath::find()
            .filter(CCachedPath::Hash.eq(nar.store_hash.clone()))
            .one(tx)
            .await?
            .ok_or_else(|| WebError::internal("cached_path row missing after race")),
        Err(err) => Err(err.into()),
    }
}

async fn queue_signature_placeholders<C: ConnectionTrait>(
    tx: &C,
    cached_path: &gradient_entity::cached_path::Model,
    org: &gradient_types::ids::OrganizationId,
) -> WebResult<()> {
    let org_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(*org))
        .all(tx)
        .await?;
    if org_caches.is_empty() {
        return Ok(());
    }

    let now_ts = now();
    let rows: Vec<ACachedPathSignature> = org_caches
        .into_iter()
        .map(|oc| {
            MCachedPathSignature {
                id: CachedPathSignatureId::now_v7(),
                cached_path: cached_path.id,
                cache: oc.cache,
                created_at: now_ts,
                ..Default::default()
            }
            .into_active_model()
        })
        .collect();

    let _ = ECachedPathSignature::insert_many(rows)
        .on_conflict(
            OnConflict::columns([
                CCachedPathSignature::CachedPath,
                CCachedPathSignature::Cache,
            ])
            .do_nothing()
            .to_owned(),
        )
        .do_nothing()
        .exec(tx)
        .await?;
    Ok(())
}

async fn ensure_build_request_project<C: ConnectionTrait>(
    tx: &C,
    org_id: gradient_types::ids::OrganizationId,
    user_id: gradient_types::ids::UserId,
) -> WebResult<gradient_entity::project::Model> {
    if let Some(existing) = EProject::find()
        .filter(
            Condition::all()
                .add(CProject::Organization.eq(org_id))
                .add(CProject::Name.eq(BUILD_REQUEST_PROJECT_NAME)),
        )
        .one(tx)
        .await?
    {
        return Ok(existing);
    }

    let project = MProject {
        id: ProjectId::now_v7(),
        organization: org_id,
        name: BUILD_REQUEST_PROJECT_NAME.to_string(),
        active: true,
        display_name: "Build Requests".to_string(),
        description: "Server-managed project for `gradient build` submissions.".to_string(),
        repository: BUILD_REQUEST_PROJECT_NAME.to_string(),
        wildcard: "*".to_string(),
        last_check_at: *NULL_TIME,
        created_by: user_id,
        created_at: now(),
        managed: true,
        keep_evaluations: 30,
        concurrency: ConcurrencyPolicy::SoftAbort,
        sign_cache: true,
        ..Default::default()
    }
    .into_active_model();

    match project.insert(tx).await {
        Ok(row) => Ok(row),
        Err(err) if is_unique_violation(&err) => EProject::find()
            .filter(
                Condition::all()
                    .add(CProject::Organization.eq(org_id))
                    .add(CProject::Name.eq(BUILD_REQUEST_PROJECT_NAME)),
            )
            .one(tx)
            .await?
            .ok_or_else(|| WebError::internal("build-request project missing after race")),
        Err(err) => Err(err.into()),
    }
}

fn is_unique_violation(err: &DbErr) -> bool {
    let sqlx_err = match err {
        DbErr::Query(RuntimeErr::SqlxError(e)) | DbErr::Exec(RuntimeErr::SqlxError(e)) => e,
        _ => return false,
    };
    matches!(
        sqlx_err,
        sqlx::Error::Database(db_err) if db_err.is_unique_violation()
    )
}
