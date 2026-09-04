/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /build-requests/{session}/dispatch` - finalises a build-request
//! upload session by materialising the staged blobs into a
//! `/nix/store/<hash>-source` path, persisting `cached_path` metadata,
//! lazily creating a per-project `build-request` task, and queueing an
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
use gradient_proto::ingest::{NarCommit, SignTargets};
use gradient_storage::source_nar::{SourceNar, materialise_source_nar};
use gradient_types::ConcurrencyPolicy;
use gradient_types::ids::{
    CommitId, EvaluationFlakeInputOverrideId, EvaluationId, TaskId, UploadSessionId,
};
use gradient_types::{
    AEvaluationFlakeInputOverride, AUploadSession, BaseResponse, CProjectCache, CTask, ECache,
    EEvaluationFlakeInputOverride, EProjectCache, ETask, EUploadSession, MCommit, MEvaluation,
    MEvaluationFlakeInputOverride, MTask, MUser, NULL_TIME, now,
};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, RuntimeErr, TransactionTrait, sqlx,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::fs;

const BUILD_REQUEST_TASK_NAME: &str = "build-request";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InputOverrideBody {
    pub input_name: String,
    pub url: String,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct DispatchRequest {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub input_overrides: Vec<InputOverrideBody>,
}

const REMOTE_OVERRIDE_SCHEMES: &[&str] = &[
    "github:",
    "gitlab:",
    "sourcehut:",
    "git+ssh://",
    "git+https://",
    "git+http://",
    "git://",
    "https://",
    "http://",
    "flake:",
];

/// Defense in depth for `--override-input`: the CLI validates too, but the REST
/// API is directly callable. gradient evaluates on the server, so only remote
/// flake refs (and fetchable `/nix/store` paths) are accepted.
pub(super) fn validate_remote_override(input_name: &str, url: &str) -> WebResult<()> {
    let mut chars = input_name.chars();
    let name_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !name_ok {
        return Err(WebError::bad_request(
            "input_name must match ^[A-Za-z_][A-Za-z0-9_-]*$",
        ));
    }

    let ref_ok = REMOTE_OVERRIDE_SCHEMES.iter().any(|s| url.starts_with(s))
        || url.starts_with("path:/nix/store/");
    if !ref_ok {
        return Err(WebError::bad_request(format!(
            "override-input '{input_name}': '{url}' is not a remote flake ref; \
             use github:/gitlab:/git+ssh://flake:/path:/nix/store/... ."
        )));
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DispatchResponse {
    pub evaluation: EvaluationId,
    pub task: TaskId,
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
        session.project,
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
        &session.project.into_inner(),
        &manifest,
        staging.path(),
    )
    .await?;

    let nar = materialise_source_nar(staging.path())
        .await
        .map_err(|e| WebError::internal(format!("Failed to materialise source NAR: {}", e)))?;

    let input_overrides = body
        .input_overrides
        .into_iter()
        .map(|o| (o.input_name, o.url))
        .collect();

    let response = finalize_build_request(
        &state,
        session.project,
        &user,
        &nar,
        body.target,
        body.system,
        input_overrides,
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
    project: gradient_types::ids::ProjectId,
    user: &MUser,
    nar: &SourceNar,
    target: Option<String>,
    system: Option<String>,
    input_overrides: Vec<(String, String)>,
) -> WebResult<DispatchResponse> {
    let _ = system;

    for (input_name, url) in &input_overrides {
        validate_remote_override(input_name, url)?;
    }

    state
        .nar_storage
        .put(&nar.store_hash, nar.compressed_bytes.clone())
        .await
        .map_err(|e| WebError::internal(format!("Failed to store source NAR: {}", e)))?;

    state
        .graph
        .commit_nar(NarCommit {
            store_path: format!("{}-source", nar.store_hash),
            file_hash: nar.file_hash_sri.clone(),
            file_size: nar.file_size as i64,
            nar_size: nar.nar_size as i64,
            nar_hash: nar.nar_hash_sri.clone(),
            references: Vec::new(),
            deriver: None,
            ca: None,
            targets: SignTargets::ProjectCaches(project),
        })
        .await
        .map_err(|e| WebError::internal(format!("failed to record source NAR: {e}")))?;

    let tx = state.web_db.inner().begin().await?;
    let response = queue_build_request(
        &tx,
        state,
        project,
        user,
        BuildRequestSource {
            repository: nar.store_path.clone(),
            commit_hash: vec![0; 20],
            commit_message: format!("Build request {}", nar.store_hash),
            target,
            input_overrides,
        },
    )
    .await?;

    tx.commit().await?;

    Ok(response)
}

/// What the evaluator should read, and how to label it. `repository` is either a
/// `/nix/store/<hash>-source` path for an uploaded source or a `git+…?rev=` URL
/// for a remote one; the evaluator treats both as flake sources.
pub(super) struct BuildRequestSource {
    pub repository: String,
    /// Real commit hash for a remote source; the upload path has no commit and
    /// passes a zero placeholder.
    pub commit_hash: Vec<u8>,
    pub commit_message: String,
    pub target: Option<String>,
    pub input_overrides: Vec<(String, String)>,
}

/// Queues one evaluation on the project's reserved `build-request` task,
/// creating that task on first use. Shared by every build-request entry point.
///
/// Evaluations are marked `concurrent`: build requests are independent one-shot
/// jobs, so they must not serialise behind each other on
/// `uq_evaluation_one_active_per_task`, nor abort one another.
pub(super) async fn queue_build_request<C: ConnectionTrait>(
    tx: &C,
    state: &ServerState,
    project: gradient_types::ids::ProjectId,
    user: &MUser,
    source: BuildRequestSource,
) -> WebResult<DispatchResponse> {
    let task = ensure_build_request_task(
        tx,
        project,
        user.id,
        state.config.storage.default_keep_evaluations(),
    )
    .await?;

    let target = source
        .target
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| task.wildcard.clone());

    let commit = MCommit {
        id: CommitId::now_v7(),
        message: source.commit_message,
        hash: source.commit_hash,
        author: Some(user.id),
        author_name: user.name.clone(),
    }
    .into_active_model()
    .insert(tx)
    .await?;

    let now_ts = now();
    let evaluation = MEvaluation {
        id: EvaluationId::now_v7(),
        task: Some(task.id),
        repository: source.repository,
        commit: commit.id,
        wildcard: target,
        status: gradient_entity::evaluation::EvaluationStatus::Queued,
        concurrent: true,
        created_at: now_ts,
        updated_at: now_ts,
        ..Default::default()
    }
    .into_active_model()
    .insert(tx)
    .await?;

    if !source.input_overrides.is_empty() {
        let rows: Vec<AEvaluationFlakeInputOverride> = source
            .input_overrides
            .into_iter()
            .map(|(input_name, url)| {
                MEvaluationFlakeInputOverride {
                    id: EvaluationFlakeInputOverrideId::now_v7(),
                    evaluation: evaluation.id,
                    input_name,
                    url: Some(url),
                }
                .into_active_model()
            })
            .collect();
        EEvaluationFlakeInputOverride::insert_many(rows)
            .exec(tx)
            .await?;
    }

    let cache = resolve_project_cache_name(tx, project).await?;

    Ok(DispatchResponse {
        evaluation: evaluation.id,
        task: task.id,
        commit: commit.id,
        cache,
    })
}

async fn resolve_project_cache_name<C: ConnectionTrait>(
    tx: &C,
    project: gradient_types::ids::ProjectId,
) -> WebResult<Option<String>> {
    let Some(link) = EProjectCache::find()
        .filter(CProjectCache::Project.eq(project))
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
    project_uuid: &uuid::Uuid,
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
            .get_blob(*project_uuid, &hash_array)
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

async fn ensure_build_request_task<C: ConnectionTrait>(
    tx: &C,
    project_id: gradient_types::ids::ProjectId,
    user_id: gradient_types::ids::UserId,
    keep_evaluations: i32,
) -> WebResult<gradient_entity::task::Model> {
    if let Some(existing) = ETask::find()
        .filter(
            Condition::all()
                .add(CTask::Project.eq(project_id))
                .add(CTask::Name.eq(BUILD_REQUEST_TASK_NAME)),
        )
        .one(tx)
        .await?
    {
        return Ok(existing);
    }

    let task = MTask {
        id: TaskId::now_v7(),
        project: project_id,
        name: BUILD_REQUEST_TASK_NAME.to_string(),
        active: true,
        display_name: "Build Requests".to_string(),
        description: "Server-managed task for `gradient build` submissions.".to_string(),
        repository: BUILD_REQUEST_TASK_NAME.to_string(),
        wildcard: "*".to_string(),
        last_check_at: *NULL_TIME,
        created_by: user_id,
        created_at: now(),
        managed: true,
        keep_evaluations,
        concurrency: ConcurrencyPolicy::All,
        sign_cache: true,
        ..Default::default()
    }
    .into_active_model();

    match task.insert(tx).await {
        Ok(row) => Ok(row),
        Err(err) if is_unique_violation(&err) => ETask::find()
            .filter(
                Condition::all()
                    .add(CTask::Project.eq(project_id))
                    .add(CTask::Name.eq(BUILD_REQUEST_TASK_NAME)),
            )
            .one(tx)
            .await?
            .ok_or_else(|| WebError::internal("build-request task missing after race")),
        Err(err) => Err(err.into()),
    }
}

fn is_unique_violation(err: &DbErr) -> bool {
    let sqlx_err = match err {
        DbErr::Query(RuntimeErr::SqlxError(e)) | DbErr::Exec(RuntimeErr::SqlxError(e)) => {
            e.as_ref()
        }
        _ => return false,
    };
    matches!(
        sqlx_err,
        sqlx::Error::Database(db_err) if db_err.is_unique_violation()
    )
}

#[cfg(test)]
mod tests {
    use super::validate_remote_override;

    #[test]
    fn accepts_remote_flake_refs() {
        for url in [
            "github:NixOS/nixpkgs",
            "gitlab:group/task",
            "sourcehut:~user/task",
            "git+ssh://git@h/x.git",
            "git+https://h/x.git",
            "git+http://h/x.git",
            "git://h/x.git",
            "https://h/x.tar.gz",
            "http://h/x.tar.gz",
            "flake:nixpkgs",
            "path:/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-src",
        ] {
            assert!(validate_remote_override("nixpkgs", url).is_ok(), "{url}");
        }
    }

    #[test]
    fn rejects_local_and_non_remote_refs() {
        for url in ["/abs", "./rel", "~/x", "name", "path:/home/u/x"] {
            assert!(validate_remote_override("nixpkgs", url).is_err(), "{url}");
        }
    }

    #[test]
    fn rejects_invalid_input_name() {
        assert!(validate_remote_override("1bad", "github:a/b").is_err());
        assert!(validate_remote_override("has space", "github:a/b").is_err());
        assert!(validate_remote_override("", "github:a/b").is_err());
        assert!(validate_remote_override("ok_name-1", "github:a/b").is_ok());
    }
}
