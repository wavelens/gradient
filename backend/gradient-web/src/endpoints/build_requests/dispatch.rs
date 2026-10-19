/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::BuildRequestRefusal;
use super::types::ManifestEntry;
use crate::access::has_permission;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::lookup::require;
use crate::permissions::Permission;
use crate::resource::Resource;
use anyhow::Context;
use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_db::lookup::by_id;
use gradient_graph::Graph;
use gradient_proto::import::{NarCommit, SignTargets};
use gradient_storage::NarStore;
use gradient_storage::source_nar::{SourceNar, materialise_source_nar};
use gradient_types::RuntimeConfig;
use gradient_types::ids::{
    CommitId, EvaluationFlakeInputOverrideId, EvaluationId, TaskId, UploadSessionId,
};
use gradient_types::input::{check_flake_input_override, check_manifest_path, decode_blake3_hex};
use gradient_types::{
    BaseResponse, ECache, EUploadSession, MCommit, MEvaluation, MEvaluationFlakeInputOverride,
    MUploadSession, MUser, MappingContext, OriginalStorePath, StorePath, StorePathMapper, now,
};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::fs;

#[derive(Serialize, Deserialize, Debug, Clone, utoipa::ToSchema)]
pub struct InputOverrideBody {
    pub input_name: String,
    pub url: String,
}

#[derive(Serialize, Deserialize, Debug, Default, utoipa::ToSchema)]
pub struct DispatchRequest {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub input_overrides: Vec<InputOverrideBody>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct DispatchResponse {
    pub evaluation: EvaluationId,
    pub task: TaskId,
    pub commit: CommitId,
    pub cache: Option<String>,
}

#[utoipa::path(
    post,
    path = "/build-requests/{session}/dispatch",
    operation_id = "postBuildRequestDispatch",
    tag = "build-requests",
    summary = "Finalise an upload session and queue an evaluation",
    description = "Step 3. After all `missing` blobs have been uploaded, the client posts\nhere to materialise `/nix/store/<hash>-source` from the staged files,\nsign it with the project's cache key, lazily create the per-project reserved\n`build-request` task on first use, and queue a new evaluation\ntargeting the supplied `target` attribute path (or the task's\nwildcard when omitted).",
    params(("session" = UploadSessionId, Path)),
    request_body = DispatchRequest,
    responses((status = OK, description = "Evaluation queued", body = DispatchResponse), WebError)
)]
pub async fn post_dispatch(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(session_id): Path<UploadSessionId>,
    Json(body): Json<DispatchRequest>,
) -> WebResult<Json<BaseResponse<DispatchResponse>>> {
    let session = require::<EUploadSession>(&state.web_db, session_id).await?;

    if !has_permission(
        &state.web_db,
        user.id,
        session.project,
        Permission::TriggerEvaluation,
        api_key.as_ref(),
    )
    .await?
    {
        return Err(WebError::not_found(Resource::UploadSession));
    }

    if session.dispatched_at.is_some() {
        return Err(BuildRequestRefusal::UploadSessionAlreadyDispatched.into());
    }

    if now() > session.expires_at {
        return Err(BuildRequestRefusal::UploadSessionExpired.into());
    }

    let missing: Vec<String> =
        serde_json::from_value(session.missing.clone()).context("corrupt session.missing JSON")?;
    if !missing.is_empty() {
        return Err(BuildRequestRefusal::BlobsStillMissing {
            count: missing.len(),
        }
        .into());
    }

    let manifest: Vec<ManifestEntry> = serde_json::from_value(session.manifest.clone())
        .context("corrupt session.manifest JSON")?;

    let staging = TempDir::new().context("create staging dir")?;

    materialise_staging(
        &state.nar_storage,
        &session.project.into_inner(),
        &manifest,
        staging.path(),
    )
    .await?;

    let nar = materialise_source_nar(staging.path())
        .await
        .context("materialise source NAR")?;

    let input_overrides = body
        .input_overrides
        .into_iter()
        .map(|o| (o.input_name, o.url))
        .collect();

    let project = session.project;
    let response = finalize_build_request(
        &state.db(),
        &state.graph,
        project,
        &user,
        &nar,
        BuildRequestOptions {
            target: body.target,
            input_overrides,
            session: Some(session),
        },
    )
    .await?;

    Ok(ok_json(response))
}

pub(super) struct BuildRequestOptions {
    pub target: Option<String>,
    pub input_overrides: Vec<(String, String)>,
    pub session: Option<MUploadSession>,
}

pub(super) async fn finalize_build_request(
    db: &DbContext,
    graph: &Arc<Graph>,
    project: gradient_types::ids::ProjectId,
    user: &MUser,
    nar: &SourceNar,
    options: BuildRequestOptions,
) -> WebResult<DispatchResponse> {
    for (input_name, url) in &options.input_overrides {
        check_flake_input_override(input_name, url)?;
    }

    let mapper = db.store_path_mapper(MappingContext::Project(project));
    let stored = stored_source_path(&mapper, &nar.store_path)?;
    db.storage
        .nar_storage
        .put(stored.hash(), nar.compressed_bytes.clone())
        .await
        .context("store source NAR")?;

    graph
        .commit_nar(NarCommit {
            store_path: stored.base(),
            file_hash: nar.file_hash_sri.clone(),
            file_size: nar.file_size as i64,
            nar_size: nar.nar_size as i64,
            nar_hash: nar.nar_hash_sri.clone(),
            targets: SignTargets::ProjectCaches(project),
            confirmed: true,
            ..Default::default()
        })
        .await
        .context("record source NAR")?;

    let tx = db.web_db.inner().begin().await?;
    let response = queue_build_request(
        &tx,
        &db.config,
        project,
        user,
        BuildRequestSource {
            repository: stored.full(),
            commit_hash: vec![0; 20],
            commit_message: format!("Build request {}", nar.store_hash),
            target: options.target,
            input_overrides: options.input_overrides,
        },
    )
    .await?;

    if let Some(session) = options.session
        && gradient_db::upload_sessions::mark_dispatched(&tx, session, now())
            .await?
            .is_none()
    {
        return Err(BuildRequestRefusal::UploadSessionAlreadyDispatched.into());
    }

    tx.commit().await?;

    Ok(response)
}

fn stored_source_path(mapper: &StorePathMapper, store_path: &str) -> anyhow::Result<StorePath> {
    let original = OriginalStorePath::parse(store_path).context("source NAR store path")?;
    Ok(mapper.inbound(&original))
}

pub(super) struct BuildRequestSource {
    pub repository: String,
    pub commit_hash: Vec<u8>,
    pub commit_message: String,
    pub target: Option<String>,
    pub input_overrides: Vec<(String, String)>,
}

pub(super) async fn queue_build_request<C: ConnectionTrait>(
    tx: &C,
    config: &RuntimeConfig,
    project: gradient_types::ids::ProjectId,
    user: &MUser,
    source: BuildRequestSource,
) -> WebResult<DispatchResponse> {
    let task = gradient_db::build_request_task::ensure_build_request_task(
        tx,
        project,
        user.id,
        config.eval.default_keep_evaluations(),
    )
    .await?;

    let target = source
        .target
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| task.wildcard.clone());

    let commit = gradient_db::commits::insert(
        tx,
        MCommit {
            id: CommitId::now_v7(),
            message: source.commit_message,
            hash: source.commit_hash,
            author: Some(user.id),
            author_name: user.name.clone(),
        },
    )
    .await?;

    let now_ts = now();
    let evaluation = gradient_db::evaluations::insert(
        tx,
        MEvaluation {
            id: EvaluationId::now_v7(),
            task: Some(task.id),
            repository: source.repository,
            commit: commit.id,
            wildcard: target,
            status: gradient_entity::evaluation::EvaluationStatus::Queued,
            concurrent: true,
            branch: gradient_types::branch_key::NO_BRANCH.to_owned(),
            created_at: now_ts,
            updated_at: now_ts,
            ..Default::default()
        },
    )
    .await?;

    if !source.input_overrides.is_empty() {
        let rows: Vec<MEvaluationFlakeInputOverride> = source
            .input_overrides
            .into_iter()
            .map(|(input_name, url)| MEvaluationFlakeInputOverride {
                id: EvaluationFlakeInputOverrideId::now_v7(),
                evaluation: evaluation.id,
                input_name,
                url: Some(url),
            })
            .collect();
        gradient_db::evaluations::flake_input_overrides::insert_many(tx, rows).await?;
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
    let Some(link) = gradient_db::projects::caches::first_subscription_of(tx, project).await?
    else {
        return Ok(None);
    };

    Ok(by_id::<ECache, _>(tx, link.cache)
        .await?
        .filter(|c| c.active)
        .map(|c| c.name))
}

async fn materialise_staging(
    nar_storage: &NarStore,
    project_uuid: &uuid::Uuid,
    manifest: &[ManifestEntry],
    root: &std::path::Path,
) -> WebResult<()> {
    for entry in manifest {
        check_manifest_path(&entry.path)?;
        let hash_bytes = decode_blake3_hex(&entry.hash)?;
        let mut hash_array = [0u8; 32];
        hash_array.copy_from_slice(&hash_bytes);

        let data = nar_storage
            .get_blob(*project_uuid, &hash_array)
            .await
            .context("fetch blob")?
            .with_context(|| format!("blob {} disappeared from storage", entry.hash))?;

        let target = root.join(&entry.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).await.context("create dir")?;
        }

        fs::write(&target, data)
            .await
            .with_context(|| format!("write {}", entry.path))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
    use gradient_types::ProjectId;

    #[test]
    fn a_source_nar_is_stored_under_its_mapped_path() {
        let mapper = StorePathMapper::new(
            Arc::new(RotatedHashMapping),
            MappingContext::Project(ProjectId::nil()),
        );

        let stored = stored_source_path(
            &mapper,
            &format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-source"),
        )
        .expect("stored");

        assert_eq!(
            stored.full(),
            format!("/nix/store/{EXAMPLE_STORED_HASH}-source")
        );
    }
}
