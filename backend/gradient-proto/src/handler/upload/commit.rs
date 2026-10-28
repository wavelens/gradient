/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::cache_paths::project_write_caches;
use gradient_db::eval_cache::record_segment;
use gradient_db::exec_jobs::{continuations, outputs};
use gradient_db::{DbContext, WorkerDb};
use gradient_storage::admission::{ObjectKey, UploadPermit};
use gradient_storage::{NarStore, StagedFile};
use gradient_types::exec::ContinueFile;
use gradient_types::ids::{CacheId, ExecJobId, ProjectId};
use gradient_types::{EExecJob, RuntimeConfig};
use gradient_wire::messages::ServerMessage;
use gradient_wire::types::{
    CONTINUE_OUTPUT, NarUploadMetadata, UploadMetadata, UploadObject, UploadOutcome,
};
use sea_orm::EntityTrait;
use tracing::warn;

use super::super::SessionHandles;
use super::super::nar::{NarUploadRecord, mark_nar_stored, record_nar_push_metric};
use super::super::socket::{ProtoWriter, send_server_msg};
use super::super::store_paths::{inbound_upload_metadata, unmapped_project};
use super::table::Transfer;
use crate::import::{WriteNeeded, nar_write_needed};

pub(super) struct Commit {
    pub writer: ProtoWriter,
    pub handles: SessionHandles,
    pub peer_id: String,
    pub request_id: u64,
    pub project: ProjectId,
    pub key: ObjectKey,
    pub segment_cache: Option<CacheId>,
    pub object: UploadObject,
    pub transfer: Transfer,
    pub metadata: UploadMetadata,
    pub permit: UploadPermit,
}

/// The byte budget is returned once the object is stored. The path stays claimed until the graph
/// recorded it, and a second upload of the path is waiting instead of overwriting the object.
#[tracing::instrument(name = "upload_commit", level = "debug", skip_all)]
pub(super) async fn run(c: Commit) {
    let metadata = match c.handles.db.project_mapper(c.project).await {
        Ok(mapper) => inbound_upload_metadata(c.metadata, &mapper).map_err(|e| e.to_string()),
        Err(e) => Err(unmapped_project(&e)),
    };
    let placed = match metadata {
        Ok(metadata) => place(&c.handles.db, &c.key, &c.object, c.transfer, &metadata)
            .await
            .map(|continue_file| (metadata, continue_file)),
        Err(reason) => Err(rejected(reason)),
    };
    let outcome = match placed {
        Ok((metadata, continue_file)) => {
            let mut permit = c.permit;
            permit.release_budget();
            let outcome = record(
                &c.handles,
                c.project,
                c.segment_cache,
                &c.object,
                &metadata,
                continue_file,
            )
            .await;
            if matches!(outcome, UploadOutcome::Ok) {
                permit.committed();
            }

            outcome
        }
        Err(outcome) => {
            drop(c.permit);
            outcome
        }
    };
    if !matches!(outcome, UploadOutcome::Ok) {
        warn!(peer_id = %c.peer_id, request_id = c.request_id, ?outcome, "upload commit did not land");
    }
    let _ = send_server_msg(
        &c.writer,
        &ServerMessage::UploadCommitted {
            request_id: c.request_id,
            outcome,
        },
    )
    .await;
}

fn retry(e: impl std::fmt::Display) -> UploadOutcome {
    UploadOutcome::Retry {
        reason: e.to_string(),
    }
}

fn rejected(reason: String) -> UploadOutcome {
    UploadOutcome::Rejected { reason }
}

#[tracing::instrument(name = "place_upload", level = "debug", skip_all)]
async fn place(
    db: &DbContext,
    key: &ObjectKey,
    object: &UploadObject,
    transfer: Transfer,
    metadata: &UploadMetadata,
) -> Result<Option<ContinueFile>, UploadOutcome> {
    match (object, metadata) {
        (UploadObject::Nar { store_path }, UploadMetadata::Nar(meta)) => {
            let Some(hash) = super::nar_key(store_path) else {
                return Err(rejected(format!("malformed store path {store_path}")));
            };
            match transfer {
                Transfer::Passthrough(writer) => {
                    place_passed_through(
                        &db.worker_db,
                        &db.storage.nar_storage,
                        &hash,
                        *writer,
                        meta,
                    )
                    .await
                }
                Transfer::Put | Transfer::Multipart { .. } => {
                    place_presigned(&db.storage.nar_storage, &db.config, &hash, meta).await
                }
            }
            .map(|()| None)
        }
        (UploadObject::EvalCacheSegment { .. }, UploadMetadata::EvalCache { size_bytes }) => {
            let ObjectKey::EvalCache(key) = key else {
                return Err(rejected("the upload holds no eval cache key".into()));
            };
            place_eval_cache(&db.storage.nar_storage, key, transfer, *size_bytes)
                .await
                .map(|()| None)
        }
        (UploadObject::ExecOutput { job, path }, UploadMetadata::ExecOutput { size_bytes }) => {
            let job = exec_job_id(job)?;
            place_exec_output(&db.storage.nar_storage, job, path, transfer, *size_bytes).await
        }
        _ => Err(rejected("metadata does not match the upload object".into())),
    }
}

#[tracing::instrument(name = "upload_graph_commit", level = "debug", skip_all)]
async fn record(
    handles: &SessionHandles,
    project_id: ProjectId,
    segment_cache: Option<CacheId>,
    object: &UploadObject,
    metadata: &UploadMetadata,
    continue_file: Option<ContinueFile>,
) -> UploadOutcome {
    match (object, metadata, segment_cache) {
        (UploadObject::Nar { store_path }, UploadMetadata::Nar(meta), _) => {
            record_nar(handles, project_id, store_path, meta).await
        }
        (UploadObject::ExecOutput { job, path }, UploadMetadata::ExecOutput { size_bytes }, _) => {
            match exec_job_id(job) {
                Ok(job) => record_exec_output(handles, job, path, *size_bytes, continue_file).await,
                Err(outcome) => outcome,
            }
        }
        (
            UploadObject::EvalCacheSegment { fingerprint },
            UploadMetadata::EvalCache { .. },
            Some(cache),
        ) => record_eval_cache_segment(handles, project_id, cache, fingerprint).await,
        (UploadObject::EvalCacheSegment { .. }, _, None) => {
            rejected("the upload holds no eval cache".into())
        }
        _ => UploadOutcome::Ok,
    }
}

async fn record_nar(
    handles: &SessionHandles,
    project_id: ProjectId,
    store_path: &str,
    meta: &NarUploadMetadata,
) -> UploadOutcome {
    let record = NarUploadRecord {
        file_hash: &meta.file_hash,
        file_size: meta.file_size as i64,
        nar_size: meta.nar_size as i64,
        nar_hash: &meta.nar_hash,
        references: &meta.references,
        deriver: meta.deriver.as_deref(),
        ca: meta.ca.as_deref(),
        confirmed: true,
    };
    if let Err(e) =
        mark_nar_stored(&handles.graph, &handles.db, project_id, store_path, &record).await
    {
        return retry(format!("recording {store_path} in the cache index: {e:#}"));
    }

    let _ = record_nar_push_metric(
        &handles.db.worker_db,
        &handles.cache_traffic,
        project_id,
        meta.file_size as i64,
    )
    .await;
    UploadOutcome::Ok
}

async fn record_eval_cache_segment(
    handles: &SessionHandles,
    project_id: ProjectId,
    cache: CacheId,
    fingerprint: &str,
) -> UploadOutcome {
    let db = &handles.db.worker_db;
    let copies: Vec<CacheId> = match project_write_caches(db, project_id).await {
        Ok(writable) => writable.into_iter().filter(|&c| c != cache).collect(),
        Err(e) => {
            warn!(%project_id, error = %e, "writable caches lookup failed; recording without copies");
            Vec::new()
        }
    };

    match record_segment(db, cache, fingerprint, &copies).await {
        Ok(()) => UploadOutcome::Ok,
        Err(e) => retry(format!("recording the eval cache segment: {e}")),
    }
}

async fn place_passed_through(
    db: &WorkerDb,
    nar_storage: &NarStore,
    hash: &str,
    writer: gradient_storage::PartialWriter,
    meta: &NarUploadMetadata,
) -> Result<(), UploadOutcome> {
    let resumed = writer.resumed();
    let staged = writer.finish().await.map_err(retry)?;
    let mismatch = if staged.len != meta.file_size {
        Some(format!(
            "received {} bytes, reported {}",
            staged.len, meta.file_size
        ))
    } else if !gradient_storage::file_hash_matches(&meta.file_hash, &staged.sha256) {
        Some(format!("received bytes are not {}", meta.file_hash))
    } else {
        None
    };
    if let Some(reason) = mismatch {
        let _ = tokio::fs::remove_file(&staged.path).await;
        return Err(if resumed {
            retry(reason)
        } else {
            rejected(reason)
        });
    }

    let needed = nar_write_needed(db, nar_storage, hash, &meta.file_hash).await;
    if !matches!(needed, Ok(WriteNeeded::Write)) {
        let _ = tokio::fs::remove_file(&staged.path).await;
        needed.map_err(retry)?;
        return Ok(());
    }

    nar_storage
        .adopt_file(hash, &staged.path)
        .await
        .map_err(retry)?;
    match staged.bytes {
        Some(bytes) => nar_storage.hot().insert(hash, bytes),
        None => nar_storage.hot().invalidate(hash),
    }
    Ok(())
}

async fn place_presigned(
    nar_storage: &NarStore,
    config: &RuntimeConfig,
    hash: &str,
    meta: &NarUploadMetadata,
) -> Result<(), UploadOutcome> {
    if let Some(receipt) = &meta.multipart
        && let Err(e) = nar_storage.complete_multipart(hash, receipt).await
    {
        nar_storage.abort_multipart(hash, &receipt.upload_id).await;
        return Err(retry(format!("completing the multipart upload: {e:#}")));
    }
    nar_storage
        .verify(
            hash,
            &meta.file_hash,
            meta.file_size,
            config.nar.verify_digest,
        )
        .await
        .map_err(retry)?;
    nar_storage.hot().invalidate(hash);
    Ok(())
}

async fn place_eval_cache(
    nar_storage: &NarStore,
    key: &str,
    transfer: Transfer,
    size_bytes: u64,
) -> Result<(), UploadOutcome> {
    match transfer {
        Transfer::Passthrough(writer) => {
            let staged = (*writer).finish().await.map_err(retry)?;
            if staged.len != size_bytes {
                return Err(rejected(format!(
                    "received {} bytes, reported {size_bytes}",
                    staged.len
                )));
            }
            let bytes = tokio::fs::read(&staged.path).await.map_err(retry)?;
            let _ = tokio::fs::remove_file(&staged.path).await;
            nar_storage.put_eval_cache(key, bytes).await.map_err(retry)
        }
        Transfer::Put | Transfer::Multipart { .. } => nar_storage
            .verify_eval_cache(key, size_bytes)
            .await
            .map_err(retry),
    }
}

fn exec_job_id(job: &str) -> Result<ExecJobId, UploadOutcome> {
    job.parse()
        .map_err(|_| rejected(format!("malformed exec job id {job}")))
}

async fn place_exec_output(
    nar_storage: &NarStore,
    job: ExecJobId,
    path: &str,
    transfer: Transfer,
    size_bytes: u64,
) -> Result<Option<ContinueFile>, UploadOutcome> {
    let Transfer::Passthrough(writer) = transfer else {
        return Err(rejected("exec outputs pass through the server".into()));
    };
    let staged = (*writer).finish().await.map_err(retry)?;
    let continue_file = match check_staged_output(&staged, path, size_bytes).await {
        Ok(continue_file) => continue_file,
        Err(outcome) => {
            let _ = tokio::fs::remove_file(&staged.path).await;
            return Err(outcome);
        }
    };

    nar_storage
        .adopt_exec_output(job.into_inner(), path, &staged.path)
        .await
        .map_err(retry)?;
    Ok(continue_file)
}

async fn check_staged_output(
    staged: &StagedFile,
    path: &str,
    size_bytes: u64,
) -> Result<Option<ContinueFile>, UploadOutcome> {
    if staged.len != size_bytes {
        return Err(rejected(format!(
            "received {} bytes, reported {size_bytes}",
            staged.len
        )));
    }
    if path != CONTINUE_OUTPUT {
        return Ok(None);
    }

    let bytes = tokio::fs::read(&staged.path).await.map_err(retry)?;
    ContinueFile::parse(&bytes)
        .map(Some)
        .map_err(|e| rejected(e.to_string()))
}

async fn record_exec_output(
    handles: &SessionHandles,
    job: ExecJobId,
    path: &str,
    size: u64,
    continue_file: Option<ContinueFile>,
) -> UploadOutcome {
    let db = &handles.db.worker_db;
    let size = i64::try_from(size).unwrap_or(i64::MAX);
    if let Err(e) = outputs::upsert(db, job, path, size, &output_content_type(path)).await {
        return retry(format!("recording output {path}: {e}"));
    }

    match continue_file {
        Some(file) => record_continuation(db, job, &file).await,
        None => UploadOutcome::Ok,
    }
}

async fn record_continuation(db: &WorkerDb, job: ExecJobId, file: &ContinueFile) -> UploadOutcome {
    let evaluation = match EExecJob::find_by_id(job).one(db).await {
        Ok(Some(row)) => row.evaluation,
        Ok(None) => return rejected(format!("exec job {job} does not exist")),
        Err(e) => return retry(format!("loading exec job {job}: {e}")),
    };

    match continuations::record(db, job, evaluation, &file.attributes).await {
        Ok(true) => UploadOutcome::Ok,
        Ok(false) => rejected(format!(
            "{CONTINUE_OUTPUT}: exec job {job} is no longer running"
        )),
        Err(e) => retry(format!("recording {CONTINUE_OUTPUT}: {e}")),
    }
}

fn output_content_type(path: &str) -> String {
    mime_guess::from_path(path).first().map_or_else(
        || "application/octet-stream".to_owned(),
        |mime| mime.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::ServerState;
    use gradient_fixtures::mock::{empty_db, mock_db};
    use gradient_storage::admission::Admitted;
    use gradient_test_support::state::test_state;
    use std::sync::Arc;

    async fn granted_permit(
        state: &Arc<ServerState>,
    ) -> (
        gradient_storage::admission::AdmissionSession,
        gradient_storage::admission::UploadPermit,
    ) {
        let (session, mut rx) = state.memory.upload_admission.open_session("test");
        session.request(1, ObjectKey::Nar("c".repeat(32)), 3, false);
        let Some(Admitted::Granted { permit, .. }) = rx.recv().await else {
            panic!("granted")
        };
        (session, permit)
    }

    #[tokio::test]
    async fn a_passed_through_nar_with_the_wrong_size_is_rejected_and_frees_its_permit() {
        let state = test_state(empty_db());
        let (_session, permit) = granted_permit(&state).await;
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials.open_writer("peer/c", "c", 0, 0).await.unwrap();
        writer.append(0, b"abc").await.unwrap();
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            handles: SessionHandles::from_state(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: ProjectId::nil(),
            key: ObjectKey::Nar("c".repeat(32)),
            segment_cache: None,
            object: UploadObject::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
            },
            transfer: Transfer::Passthrough(Box::new(writer)),
            metadata: UploadMetadata::Nar(Box::new(NarUploadMetadata {
                file_hash: "sha256:0000".into(),
                file_size: 99,
                nar_size: 3,
                nar_hash: "sha256:1111".into(),
                references: Vec::new(),
                deriver: None,
                ca: None,
                multipart: None,
            })),
            permit,
        })
        .await;

        let msg = gradient_wire::codec::from_bytes::<ServerMessage>(
            sent.try_recv().unwrap(),
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .unwrap();
        assert!(matches!(
            msg,
            ServerMessage::UploadCommitted {
                request_id: 1,
                outcome: UploadOutcome::Rejected { .. }
            }
        ));
        assert_eq!(state.memory.upload_admission.in_flight(), 0);
    }

    #[tokio::test]
    async fn a_presigned_nar_that_is_not_in_storage_is_retried() {
        let state = test_state(empty_db());
        let (_session, permit) = granted_permit(&state).await;
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            handles: SessionHandles::from_state(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: ProjectId::nil(),
            key: ObjectKey::Nar("c".repeat(32)),
            segment_cache: None,
            object: UploadObject::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
            },
            transfer: Transfer::Put,
            metadata: UploadMetadata::Nar(Box::new(NarUploadMetadata {
                file_hash: "sha256:0000".into(),
                file_size: 3,
                nar_size: 3,
                nar_hash: "sha256:1111".into(),
                references: Vec::new(),
                deriver: None,
                ca: None,
                multipart: None,
            })),
            permit,
        })
        .await;

        let msg = gradient_wire::codec::from_bytes::<ServerMessage>(
            sent.try_recv().unwrap(),
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .unwrap();
        assert!(matches!(
            msg,
            ServerMessage::UploadCommitted {
                request_id: 1,
                outcome: UploadOutcome::Retry { .. }
            }
        ));
    }

    #[tokio::test]
    async fn a_passed_through_nar_for_a_stored_path_leaves_the_stored_object() {
        let hash = "s".repeat(32);
        let stored = gradient_types::MCachedPath {
            hash: hash.clone(),
            file_hash: Some("sha256:stored".into()),
            confirmed: true,
            ..Default::default()
        };
        let state = test_state(
            mock_db()
                .append_query_results([vec![stored]])
                .into_connection(),
        );
        state.nar_storage.put(&hash, b"OLD".to_vec()).await.unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials.open_writer("peer/s", "s", 0, 0).await.unwrap();
        writer.append(0, b"abc").await.unwrap();

        place(
            &state.db(),
            &ObjectKey::Nar(hash.clone()),
            &UploadObject::Nar {
                store_path: format!("/nix/store/{hash}-p"),
            },
            Transfer::Passthrough(Box::new(writer)),
            &UploadMetadata::Nar(Box::new(NarUploadMetadata {
                file_hash: gradient_storage::file_hash_sri(b"abc"),
                file_size: 3,
                nar_size: 3,
                nar_hash: "sha256:1111".into(),
                references: Vec::new(),
                deriver: None,
                ca: None,
                multipart: None,
            })),
        )
        .await
        .unwrap_or_else(|outcome| panic!("the upload settles: {outcome:?}"));

        assert_eq!(state.nar_storage.get(&hash).await.unwrap().unwrap(), b"OLD");
    }

    #[tokio::test]
    async fn a_placed_nar_frees_its_budget_but_keeps_its_path_until_the_graph_records_it() {
        let Ok(mut unwired) = Arc::try_unwrap(test_state(
            mock_db()
                .append_query_results([Vec::<gradient_types::MCachedPath>::new()])
                .into_connection(),
        )) else {
            panic!("sole owner")
        };
        unwired.graph = gradient_core::Graph::new();
        let state = Arc::new(unwired);
        let (_session, permit) = granted_permit(&state).await;
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials.open_writer("peer/c", "c", 0, 0).await.unwrap();
        writer.append(0, b"abc").await.unwrap();
        let (writer_out, _sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        let commit = run(Commit {
            writer: writer_out,
            handles: SessionHandles::from_state(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: ProjectId::nil(),
            key: ObjectKey::Nar("c".repeat(32)),
            segment_cache: None,
            object: UploadObject::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
            },
            transfer: Transfer::Passthrough(Box::new(writer)),
            metadata: UploadMetadata::Nar(Box::new(NarUploadMetadata {
                file_hash: gradient_storage::file_hash_sri(b"abc"),
                file_size: 3,
                nar_size: 3,
                nar_hash: "sha256:1111".into(),
                references: Vec::new(),
                deriver: None,
                ca: None,
                multipart: None,
            })),
            permit,
        });
        tokio::pin!(commit);

        let freed = async {
            while state.memory.upload_admission.in_flight() != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                () = &mut commit => panic!("the graph has not recorded the upload yet"),
                () = freed => {}
            }
        })
        .await
        .expect("the budget is back once the bytes are stored");

        let (other, mut other_rx) = state.memory.upload_admission.open_session("other");
        other.request(2, ObjectKey::Nar("c".repeat(32)), 3, false);
        tokio::select! {
            () = &mut commit => panic!("the graph has not recorded the upload yet"),
            _ = other_rx.recv() => panic!("a second upload of the path must wait for the graph"),
            () = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
        }
    }

    #[tokio::test]
    async fn a_resumed_passthrough_that_fails_its_hash_is_retried_from_scratch() {
        let state = test_state(empty_db());
        let (_session, permit) = granted_permit(&state).await;
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        partials.append("peer/c", "c", 0, b"ab").await.unwrap();
        let mut writer = partials.open_writer("peer/c", "c", 2, 0).await.unwrap();
        assert!(writer.resumed());
        writer.append(2, b"c").await.unwrap();
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            handles: SessionHandles::from_state(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: ProjectId::nil(),
            key: ObjectKey::Nar("c".repeat(32)),
            segment_cache: None,
            object: UploadObject::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
            },
            transfer: Transfer::Passthrough(Box::new(writer)),
            metadata: UploadMetadata::Nar(Box::new(NarUploadMetadata {
                file_hash: gradient_storage::file_hash_sri(b"xyz"),
                file_size: 3,
                nar_size: 3,
                nar_hash: "sha256:1111".into(),
                references: Vec::new(),
                deriver: None,
                ca: None,
                multipart: None,
            })),
            permit,
        })
        .await;

        let msg = gradient_wire::codec::from_bytes::<ServerMessage>(
            sent.try_recv().unwrap(),
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .unwrap();
        assert!(matches!(
            msg,
            ServerMessage::UploadCommitted {
                request_id: 1,
                outcome: UploadOutcome::Retry { .. }
            }
        ));
        assert!(
            !partials.path("peer/c").exists(),
            "the foreign prefix is gone"
        );
    }

    #[tokio::test]
    async fn an_invalid_continue_file_is_rejected_with_its_reason() {
        let state = test_state(empty_db());
        let job = ExecJobId::now_v7();
        let body = br#"{"attributes":["packages.*"]}"#;
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials.open_writer("peer/o", "o", 0, 0).await.unwrap();
        writer.append(0, body).await.unwrap();

        let placed = place(
            &state.db(),
            &ObjectKey::Rest(gradient_storage::exec_output_key(
                job.into_inner(),
                CONTINUE_OUTPUT,
            )),
            &UploadObject::ExecOutput {
                job: job.to_string(),
                path: CONTINUE_OUTPUT.into(),
            },
            Transfer::Passthrough(Box::new(writer)),
            &UploadMetadata::ExecOutput {
                size_bytes: body.len() as u64,
            },
        )
        .await;

        let Err(UploadOutcome::Rejected { reason }) = placed else {
            panic!("expected a rejection, got {placed:?}")
        };
        assert!(reason.contains("packages.*"), "{reason}");
        assert!(
            state
                .nar_storage
                .get_exec_output(job.into_inner(), CONTINUE_OUTPUT, body.len())
                .await
                .unwrap()
                .is_none(),
            "nothing is adopted"
        );
    }

    #[tokio::test]
    async fn a_committed_output_is_recorded_with_its_size_and_type() {
        let db = mock_db()
            .append_exec_results([gradient_fixtures::mock::exec_ok()])
            .into_connection();
        let log_db = db.clone();
        let state = test_state(db);

        let outcome = record_exec_output(
            &SessionHandles::from_state(&state),
            ExecJobId::now_v7(),
            "model/w.bin",
            3,
            None,
        )
        .await;

        assert!(matches!(outcome, UploadOutcome::Ok), "{outcome:?}");
        let insert = gradient_fixtures::mock::all_statements(log_db.into_transaction_log())
            .into_iter()
            .find(|s| s.sql.starts_with("INSERT INTO \"exec_job_output\""))
            .expect("the output row is written");
        let values = format!("{:?}", insert.values);
        assert!(
            values.contains("BigInt(Some(3))") && values.contains("application/octet-stream"),
            "{values}"
        );
    }
}
