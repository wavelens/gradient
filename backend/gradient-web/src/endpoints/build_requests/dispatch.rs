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
use gradient_core::nix_hash::normalize_nar_hash;
use gradient_core::storage::source_nar::{SourceNar, materialise_source_nar};
use gradient_core::types::ConcurrencyPolicy;
use gradient_core::types::ids::{
    CachedPathId, CachedPathSignatureId, CommitId, EvaluationId, ProjectId, UploadSessionId,
};
use gradient_core::types::{
    ACachedPath, ACachedPathSignature, ACommit, AEvaluation, AProject, AUploadSession,
    BaseResponse, CCachedPath, CCachedPathSignature, COrganizationCache, CProject, ECachedPath,
    ECachedPathSignature, EOrganizationCache, EProject, EUploadSession, MUser, NULL_TIME, now,
};
use gradient_core::ServerState;
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter,
    RuntimeErr, TransactionTrait, sqlx,
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

    state
        .nar_storage
        .put(&nar.store_hash, nar.nar_bytes.clone())
        .await
        .map_err(|e| WebError::internal(format!("Failed to store source NAR: {}", e)))?;

    let tx = state.web_db.inner().begin().await?;

    let cached_path = ensure_cached_path(&tx, &nar).await?;
    queue_signature_placeholders(&tx, &cached_path, &session.organization).await?;
    let project = ensure_build_request_project(&tx, session.organization, user.id).await?;

    let target = body
        .target
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| project.wildcard.clone());

    let commit = ACommit {
        id: Set(CommitId::now_v7()),
        message: Set(format!("Build request {}", session.id)),
        hash: Set(vec![0; 20]),
        author: Set(Some(user.id)),
        author_name: Set(user.name.clone()),
    }
    .insert(&tx)
    .await?;

    let now_ts = now();
    let evaluation = AEvaluation {
        id: Set(EvaluationId::now_v7()),
        project: Set(Some(project.id)),
        repository: Set(nar.store_path.clone()),
        commit: Set(commit.id),
        wildcard: Set(target),
        status: Set(gradient_entity::evaluation::EvaluationStatus::Queued),
        previous: Set(None),
        next: Set(None),
        created_at: Set(now_ts),
        updated_at: Set(now_ts),
        flake_source: Set(None),
        check_run_ids: Set(None),
        waiting_reason: Set(None),
        trigger: Set(None),
        concurrent: Set(false),
        source_comment: Set(None),
        fetch_started_at: Set(None),
        eval_flake_started_at: Set(None),
        eval_drv_started_at: Set(None),
        building_started_at: Set(None),
        finished_at: Set(None),
    }
    .insert(&tx)
    .await?;

    tx.commit().await?;

    let mut active: AUploadSession = session.into();
    active.dispatched_at = Set(Some(now()));
    active.update(&state.web_db).await?;

    Ok(ok_json(DispatchResponse {
        evaluation: evaluation.id,
        project: project.id,
        commit: commit.id,
    }))
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

    let normalised_hash = normalize_nar_hash(&nar.nar_hash_sri);
    let row = ACachedPath {
        id: Set(CachedPathId::now_v7()),
        store_path: Set(nar.store_path.clone()),
        hash: Set(nar.store_hash.clone()),
        package: Set("source".to_string()),
        file_hash: Set(Some(normalised_hash.clone())),
        file_size: Set(Some(nar.nar_size as i64)),
        nar_size: Set(Some(nar.nar_size as i64)),
        nar_hash: Set(Some(normalised_hash)),
        references: Set(None),
        ca: Set(None),
        deriver: Set(None),
        created_at: Set(now()),
    };

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
    org: &gradient_core::types::ids::OrganizationId,
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
        .map(|oc| ACachedPathSignature {
            id: Set(CachedPathSignatureId::now_v7()),
            cached_path: Set(cached_path.id),
            cache: Set(oc.cache),
            signature: Set(None),
            created_at: Set(now_ts),
            last_fetched_at: Set(None),
            fetch_count: Set(0),
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
    org_id: gradient_core::types::ids::OrganizationId,
    user_id: gradient_core::types::ids::UserId,
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

    let project = AProject {
        id: Set(ProjectId::now_v7()),
        organization: Set(org_id),
        name: Set(BUILD_REQUEST_PROJECT_NAME.to_string()),
        active: Set(true),
        display_name: Set("Build Requests".to_string()),
        description: Set("Server-managed project for `gradient build` submissions.".to_string()),
        repository: Set(BUILD_REQUEST_PROJECT_NAME.to_string()),
        wildcard: Set("*".to_string()),
        last_evaluation: Set(None),
        last_check_at: Set(*NULL_TIME),
        force_evaluation: Set(false),
        created_by: Set(user_id),
        created_at: Set(now()),
        managed: Set(true),
        keep_evaluations: Set(30),
        concurrency: Set(i16::from(ConcurrencyPolicy::SoftAbort)),
        sign_cache: Set(true),
    };

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
