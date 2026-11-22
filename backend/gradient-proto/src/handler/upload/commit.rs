/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::str::FromStr as _;

use gradient_db::cache_paths::project_write_caches;
use gradient_db::eval_cache::{EvalCacheStoreRowKey, record_segment};
use gradient_db::exec_jobs::{continuations, outputs};
use gradient_db::{DbContext, WorkerDb};
use gradient_storage::admission::{ObjectKey, UploadPermit};
use gradient_storage::eval_cache_segment::CheckedEvalCacheFingerprint;
use gradient_storage::{
    ClientClaimedNarUploadHashesAndSizes, NarStore,
    ServerComputedNarAndCompressedFileHashesAndSizes, StagedFile,
    UploadedStorePathAndContentAddress, VerifyError,
    verify_zstd_nar_stream_sha256_and_sizes_against_claims,
};
use gradient_types::EExecJob;
use gradient_types::exec::ContinueFile;
use gradient_types::ids::{CacheId, DispatchedJobId, ExecJobId, ProjectId};
use gradient_wire::messages::ServerMessage;
use gradient_wire::types::{
    CONTINUE_OUTPUT, NarUploadMetadata, UploadMetadata, UploadObject, UploadOutcome,
};
use harmonia_store_content_address::ContentAddress;
use sea_orm::EntityTrait;
use tracing::warn;

use super::super::SessionHandles;
use super::super::nar::{NarUploadRecord, NarUploadWorkerSession, mark_nar_stored};
use super::super::socket::{ProtoWriter, send_server_msg};
use super::super::store_paths::{check_nar_upload_metadata, unmapped_project};
use super::table::{Transfer, UploadedStorePathDeriverChoice};
use gradient_types::checked_uploader_stated_store_path_metadata::CheckedUploadedStorePathReferencesDeriverAndContentAddress;

pub(super) struct Commit {
    pub writer: ProtoWriter,
    pub handles: SessionHandles,
    pub peer_id: String,
    pub request_id: u64,
    pub project: ProjectId,
    pub dispatched_job: Option<DispatchedJobId>,
    pub key: ObjectKey,
    pub segment_cache: Option<CacheId>,
    pub object: CommittedUpload,
    pub transfer: Transfer,
    pub metadata: UploadMetadata,
    pub server_granted_upload_size_bytes: u64,
    pub permit: UploadPermit,
}

pub(super) enum CommittedUpload {
    Nar {
        store_path: String,
        deriver_decision: Result<UploadedStorePathDeriverChoice, String>,
    },
    Other(UploadObject),
}

impl CommittedUpload {
    fn to_upload_object(&self) -> UploadObject {
        match self {
            Self::Nar { store_path, .. } => UploadObject::Nar {
                store_path: store_path.clone(),
            },
            Self::Other(object) => object.clone(),
        }
    }
}

/// The byte budget is returned once the object is stored. The path stays claimed until the graph
/// recorded it, and a second upload of the path is waiting instead of overwriting the object.
#[tracing::instrument(name = "upload_commit", level = "debug", skip_all)]
pub(super) async fn run(c: Commit) {
    let placed = map_upload_metadata_to_stored_form_and_place_upload(
        &c.handles.db,
        c.project,
        &c.key,
        &c.object,
        c.transfer,
        &c.metadata,
        c.server_granted_upload_size_bytes,
    )
    .await;
    let outcome = match placed {
        Ok((stated, placement)) => {
            let mut permit = c.permit;
            permit.release_budget();
            let worker = NarUploadWorkerSession {
                worker_id: &c.peer_id,
                project: c.project,
                dispatched_job: c.dispatched_job,
            };
            let outcome = record(
                &c.handles,
                c.segment_cache,
                &c.object.to_upload_object(),
                &c.metadata,
                stated.as_ref(),
                placement,
                &worker,
            )
            .await;
            drop(permit);
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

async fn map_upload_metadata_to_stored_form_and_place_upload(
    db: &DbContext,
    project: ProjectId,
    key: &ObjectKey,
    object: &CommittedUpload,
    transfer: Transfer,
    sent: &UploadMetadata,
    server_granted_upload_size_bytes: u64,
) -> Result<
    (
        Option<CheckedUploadedStorePathReferencesDeriverAndContentAddress>,
        UploadPlacementResult,
    ),
    UploadOutcome,
> {
    let temporary_upload = transfer.to_temporary_nar_upload_object();
    let placed = async {
        let (stated, client_original_store_path) = match db.project_mapper(project).await {
            Ok(mapper) => check_nar_upload_metadata(sent, &mapper)
                .map(|stated| {
                    let client_original_store_path = match object {
                        CommittedUpload::Nar { store_path, .. } => {
                            mapper.outbound_if_store_path(store_path)
                        }
                        CommittedUpload::Other(_) => String::new(),
                    };
                    (stated, client_original_store_path)
                })
                .map_err(|e| e.to_string()),
            Err(e) => Err(unmapped_project(&e)),
        }
        .map_err(rejected)?;
        let stated = match object {
            CommittedUpload::Nar {
                deriver_decision, ..
            } => match deriver_decision
                .as_ref()
                .map_err(|reason| rejected(reason.clone()))?
            {
                UploadedStorePathDeriverChoice::DispatchedBuildJobOutputDerivation {
                    dispatched_deriver_path,
                } => stated.map(|stated| {
                    stated.with_server_known_derivation_path_from_dispatched_build_job(Some(
                        dispatched_deriver_path.clone(),
                    ))
                }),
                UploadedStorePathDeriverChoice::NoRecordedDeriver => stated.map(|stated| {
                    stated.with_server_known_derivation_path_from_dispatched_build_job(None)
                }),
                UploadedStorePathDeriverChoice::UseWorkerStatedDeriver => stated,
            },
            CommittedUpload::Other(_) => stated,
        };
        let placement = place(
            db,
            key,
            &object.to_upload_object(),
            &client_original_store_path,
            transfer,
            sent,
            server_granted_upload_size_bytes,
        )
        .await?;
        Ok((stated, placement))
    }
    .await;
    if let (Err(UploadOutcome::Rejected { .. }), Some(upload)) = (&placed, temporary_upload) {
        super::abort_multipart_upload_and_delete_nar_upload_object(&db.storage.nar_storage, upload)
            .await;
    }
    placed
}

fn retry(e: impl std::fmt::Display) -> UploadOutcome {
    UploadOutcome::Retry {
        reason: e.to_string(),
    }
}

fn rejected(reason: String) -> UploadOutcome {
    UploadOutcome::Rejected { reason }
}

#[derive(Debug)]
enum UploadPlacementResult {
    Nar(ServerComputedNarAndCompressedFileHashesAndSizes),
    ExecOutput(Option<ContinueFile>),
    EvalCacheSegment,
}

fn upload_outcome_for_nar_verification_error(e: VerifyError) -> UploadOutcome {
    if e.is_caused_by_uploader() {
        rejected(e.to_string())
    } else {
        retry(e)
    }
}

fn borrow_client_claimed_nar_upload_fields<'a>(
    store_path: &'a str,
    meta: &'a NarUploadMetadata,
) -> ClientClaimedNarUploadHashesAndSizes<'a> {
    ClientClaimedNarUploadHashesAndSizes {
        file_hash: &meta.file_hash,
        file_size: meta.file_size,
        nar_hash: &meta.nar_hash,
        nar_size: meta.nar_size,
        claimed_store_path_references: &meta.references,
        uploaded_nar_with_stated_content_address_to_check: Some(
            UploadedStorePathAndContentAddress {
                store_path,
                content_address: meta
                    .ca
                    .as_deref()
                    .and_then(|ca| ContentAddress::from_str(ca).ok()),
            },
        ),
    }
}

#[tracing::instrument(name = "place_upload", level = "debug", skip_all)]
async fn place(
    db: &DbContext,
    key: &ObjectKey,
    object: &UploadObject,
    client_original_store_path: &str,
    transfer: Transfer,
    metadata: &UploadMetadata,
    server_granted_upload_size_bytes: u64,
) -> Result<UploadPlacementResult, UploadOutcome> {
    match (object, metadata) {
        (UploadObject::Nar { store_path }, UploadMetadata::Nar(meta)) => {
            let Some(hash) = super::nar_key(store_path) else {
                return Err(rejected(format!("malformed store path {store_path}")));
            };
            match transfer {
                Transfer::Passthrough(writer) => {
                    place_passed_through(
                        &db.storage.nar_storage,
                        &hash,
                        client_original_store_path,
                        *writer,
                        meta,
                    )
                    .await
                }
                Transfer::PutToTemporaryUploadObject {
                    temporary_nar_storage_key,
                } => {
                    commit_worker_upload_to_store_path(
                        db,
                        &hash,
                        client_original_store_path,
                        &temporary_nar_storage_key,
                        meta,
                        std::future::ready(()),
                    )
                    .await
                }
                Transfer::Multipart {
                    upload_id,
                    temporary_nar_storage_key,
                } => {
                    complete_and_verify_multipart_nar_upload_if_no_other_content_for_path(
                        db,
                        &hash,
                        client_original_store_path,
                        &upload_id,
                        &temporary_nar_storage_key,
                        meta,
                    )
                    .await
                }
                Transfer::Put => Err(rejected(
                    "NAR uploads need a temporary upload object.".into(),
                )),
            }
            .map(UploadPlacementResult::Nar)
        }
        (UploadObject::EvalCacheSegment { .. }, UploadMetadata::EvalCache { size_bytes }) => {
            let ObjectKey::EvalCache(key) = key else {
                return Err(rejected("the upload holds no eval cache key".into()));
            };
            place_eval_cache(
                &db.storage.nar_storage,
                key,
                transfer,
                *size_bytes,
                server_granted_upload_size_bytes,
            )
            .await
            .map(|()| UploadPlacementResult::EvalCacheSegment)
        }
        (UploadObject::ExecOutput { job, path }, UploadMetadata::ExecOutput { size_bytes }) => {
            let job = exec_job_id(job)?;
            place_exec_output(
                &db.storage.nar_storage,
                job,
                path,
                transfer,
                *size_bytes,
                server_granted_upload_size_bytes,
            )
            .await
            .map(UploadPlacementResult::ExecOutput)
        }
        _ => Err(rejected("metadata does not match the upload object".into())),
    }
}

#[tracing::instrument(name = "upload_graph_commit", level = "debug", skip_all)]
async fn record(
    handles: &SessionHandles,
    segment_cache: Option<CacheId>,
    object: &UploadObject,
    metadata: &UploadMetadata,
    stated: Option<&CheckedUploadedStorePathReferencesDeriverAndContentAddress>,
    placement: UploadPlacementResult,
    worker: &NarUploadWorkerSession<'_>,
) -> UploadOutcome {
    match (object, metadata, placement, segment_cache) {
        (
            UploadObject::Nar { store_path },
            UploadMetadata::Nar(_),
            UploadPlacementResult::Nar(verified),
            _,
        ) => match stated {
            Some(stated) => record_nar(handles, worker, store_path, stated, &verified).await,
            None => rejected("metadata does not match the upload object".into()),
        },
        (
            UploadObject::ExecOutput { job, path },
            UploadMetadata::ExecOutput { size_bytes },
            UploadPlacementResult::ExecOutput(continue_file),
            _,
        ) => match exec_job_id(job) {
            Ok(job) => record_exec_output(handles, job, path, *size_bytes, continue_file).await,
            Err(outcome) => outcome,
        },
        (
            UploadObject::EvalCacheSegment { fingerprint },
            UploadMetadata::EvalCache { .. },
            _,
            Some(cache),
        ) => record_eval_cache_segment(handles, worker.project, cache, fingerprint).await,
        (UploadObject::EvalCacheSegment { .. }, _, _, None) => {
            rejected("the upload holds no eval cache".into())
        }
        _ => UploadOutcome::Ok,
    }
}

async fn record_nar(
    handles: &SessionHandles,
    worker: &NarUploadWorkerSession<'_>,
    store_path: &str,
    stated: &CheckedUploadedStorePathReferencesDeriverAndContentAddress,
    verified: &ServerComputedNarAndCompressedFileHashesAndSizes,
) -> UploadOutcome {
    let record = NarUploadRecord {
        verified,
        references: stated.references(),
        deriver: stated.deriver(),
        ca: stated.checked_content_address_text(),
    };
    if let Err(e) = mark_nar_stored(&handles.graph, &handles.db, worker, store_path, &record).await
    {
        return retry(format!("recording {store_path} in the cache index: {e:#}"));
    }

    UploadOutcome::Ok
}

async fn record_eval_cache_segment(
    handles: &SessionHandles,
    project_id: ProjectId,
    cache: CacheId,
    fingerprint: &str,
) -> UploadOutcome {
    let fingerprint = match CheckedEvalCacheFingerprint::parse(fingerprint) {
        Ok(fingerprint) => fingerprint,
        Err(e) => return rejected(format!("{e:#}")),
    };
    let store = EvalCacheStoreRowKey {
        cache,
        project: project_id,
        fingerprint,
    };
    let db = &handles.db.worker_db;
    let copies: Vec<CacheId> = match project_write_caches(db, project_id).await {
        Ok(writable) => writable.into_iter().filter(|&c| c != cache).collect(),
        Err(e) => {
            warn!(%project_id, error = %e, "writable caches lookup failed; recording without copies");
            Vec::new()
        }
    };

    match record_segment(db, &store, &copies).await {
        Ok(()) => UploadOutcome::Ok,
        Err(e) => retry(format!("recording the eval cache segment: {e}")),
    }
}

async fn place_passed_through(
    nar_storage: &NarStore,
    hash: &str,
    store_path: &str,
    writer: gradient_storage::PartialWriter,
    meta: &NarUploadMetadata,
) -> Result<ServerComputedNarAndCompressedFileHashesAndSizes, UploadOutcome> {
    let resumed = writer.resumed();
    let staged = writer.finish().await.map_err(retry)?;
    let verified = match tokio::fs::File::open(&staged.path).await {
        Ok(file) => {
            verify_zstd_nar_stream_sha256_and_sizes_against_claims(
                file,
                &borrow_client_claimed_nar_upload_fields(store_path, meta),
            )
            .await
        }
        Err(e) => Err(VerifyError::Store(e.into())),
    };
    let verified = match verified {
        Ok(verified) => verified,
        Err(e) => {
            let _ = tokio::fs::remove_file(&staged.path).await;
            return Err(if resumed {
                retry(e)
            } else {
                upload_outcome_for_nar_verification_error(e)
            });
        }
    };

    let key = verified.object_storage_key_for_store_path_hash_and_compressed_file_sha256(hash);
    let stored = nar_storage.exists(&key).await;
    if !matches!(stored, Ok(false)) {
        let _ = tokio::fs::remove_file(&staged.path).await;
        stored.map_err(retry)?;
        return Ok(verified);
    }

    nar_storage
        .adopt_file(&key, &staged.path)
        .await
        .map_err(retry)?;
    match staged.bytes {
        Some(bytes) => nar_storage.hot().insert(&key, bytes),
        None => nar_storage.hot().invalidate(&key),
    }
    Ok(verified)
}

async fn commit_worker_upload_to_store_path(
    db: &DbContext,
    hash: &str,
    store_path: &str,
    temporary_nar_storage_key: &str,
    meta: &NarUploadMetadata,
    after_verification_future: impl Future<Output = ()>,
) -> Result<ServerComputedNarAndCompressedFileHashesAndSizes, UploadOutcome> {
    let nar_storage = &db.storage.nar_storage;
    let temporary = match nar_storage
        .move_client_upload_to_server_temporary_key(hash, temporary_nar_storage_key, meta.file_size)
        .await
    {
        Ok(temporary) => temporary,
        Err(e) => {
            if let Err(e) = nar_storage
                .delete_temporary_nar_upload(temporary_nar_storage_key)
                .await
            {
                warn!(%hash, error = %e, "Failed to delete temporary NAR upload");
            }
            return Err(upload_outcome_for_nar_verification_error(e));
        }
    };
    let checked = match nar_storage
        .read_uploaded_nar_at_temporary_key_and_check_client_hash_and_sizes(
            &temporary,
            &borrow_client_claimed_nar_upload_fields(store_path, meta),
        )
        .await
    {
        Ok(verified) => {
            let key =
                verified.object_storage_key_for_store_path_hash_and_compressed_file_sha256(hash);
            match nar_storage.exists(&key).await {
                Ok(stored) => Ok((verified, stored)),
                Err(e) => Err(retry(e)),
            }
        }
        Err(e) => Err(upload_outcome_for_nar_verification_error(e)),
    };
    if let Ok((verified, false)) = &checked {
        after_verification_future.await;
        nar_storage
            .move_verified_upload_to_final_storage_key(hash, &temporary, verified)
            .await
            .map_err(retry)?;
        return checked.map(|(verified, _)| verified);
    }

    if let Err(e) = nar_storage
        .delete_temporary_nar_upload(temporary.as_str())
        .await
    {
        warn!(%hash, error = %e, "Failed to delete temporary NAR upload");
    }
    checked.map(|(verified, _)| verified)
}

async fn complete_and_verify_multipart_nar_upload_if_no_other_content_for_path(
    db: &DbContext,
    hash: &str,
    store_path: &str,
    upload_id: &str,
    temporary_nar_storage_key: &str,
    meta: &NarUploadMetadata,
) -> Result<ServerComputedNarAndCompressedFileHashesAndSizes, UploadOutcome> {
    let nar_storage = &db.storage.nar_storage;
    let receipt = match &meta.multipart {
        Some(receipt) if receipt.upload_id == upload_id => receipt,
        Some(_) => {
            nar_storage
                .abort_multipart(temporary_nar_storage_key, upload_id)
                .await;
            return Err(rejected(
                "A multipart receipt for a different upload.".into(),
            ));
        }
        None => {
            nar_storage
                .abort_multipart(temporary_nar_storage_key, upload_id)
                .await;
            return Err(rejected(
                "The multipart upload reported no completed parts.".into(),
            ));
        }
    };
    if let Err(e) = nar_storage
        .complete_multipart(temporary_nar_storage_key, receipt)
        .await
    {
        nar_storage
            .abort_multipart(temporary_nar_storage_key, upload_id)
            .await;
        return Err(retry(format!("completing the multipart upload: {e:#}")));
    }
    commit_worker_upload_to_store_path(
        db,
        hash,
        store_path,
        temporary_nar_storage_key,
        meta,
        std::future::ready(()),
    )
    .await
}

async fn place_eval_cache(
    nar_storage: &NarStore,
    key: &str,
    transfer: Transfer,
    size_bytes: u64,
    server_granted_upload_size_bytes: u64,
) -> Result<(), UploadOutcome> {
    match transfer {
        Transfer::Passthrough(writer) => {
            let staged = (*writer).finish().await.map_err(retry)?;
            if let Err(outcome) = check_staged_upload_size_within_grant_and_matches_reported_size(
                &staged,
                size_bytes,
                server_granted_upload_size_bytes,
            ) {
                let _ = tokio::fs::remove_file(&staged.path).await;
                return Err(outcome);
            }
            let bytes = tokio::fs::read(&staged.path).await.map_err(retry)?;
            let _ = tokio::fs::remove_file(&staged.path).await;
            nar_storage.put_eval_cache(key, bytes).await.map_err(retry)
        }
        Transfer::Put
        | Transfer::PutToTemporaryUploadObject { .. }
        | Transfer::Multipart { .. } => nar_storage
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
    server_granted_upload_size_bytes: u64,
) -> Result<Option<ContinueFile>, UploadOutcome> {
    let Transfer::Passthrough(writer) = transfer else {
        return Err(rejected("exec outputs pass through the server".into()));
    };
    let staged = (*writer).finish().await.map_err(retry)?;
    let checked =
        check_staged_output(&staged, path, size_bytes, server_granted_upload_size_bytes).await;
    let continue_file = match checked {
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
    server_granted_upload_size_bytes: u64,
) -> Result<Option<ContinueFile>, UploadOutcome> {
    check_staged_upload_size_within_grant_and_matches_reported_size(
        staged,
        size_bytes,
        server_granted_upload_size_bytes,
    )?;
    if path != CONTINUE_OUTPUT {
        return Ok(None);
    }

    let bytes = tokio::fs::read(&staged.path).await.map_err(retry)?;
    ContinueFile::parse(&bytes)
        .map(Some)
        .map_err(|e| rejected(e.to_string()))
}

fn check_staged_upload_size_within_grant_and_matches_reported_size(
    staged: &StagedFile,
    size_bytes: u64,
    server_granted_upload_size_bytes: u64,
) -> Result<(), UploadOutcome> {
    if staged.len > server_granted_upload_size_bytes {
        return Err(rejected(format!(
            "received {} bytes, but the allowed size is {server_granted_upload_size_bytes} bytes",
            staged.len
        )));
    }
    if staged.len != size_bytes {
        return Err(rejected(format!(
            "received {} bytes, reported {size_bytes}",
            staged.len
        )));
    }
    Ok(())
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
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH};
    use gradient_fixtures::mock::{empty_db, mock_db};
    use gradient_storage::admission::Admitted;
    use gradient_test_support::content_address_and_store_path::{
        compute_derivation_store_path_and_text_content_address,
        flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes,
    };
    use gradient_test_support::state::test_state;
    use gradient_util::nar::single_file_nar;
    use std::sync::Arc;

    async fn granted_permit(
        state: &Arc<ServerState>,
    ) -> (
        gradient_storage::admission::AdmissionSession,
        gradient_storage::admission::UploadPermit,
    ) {
        let (session, mut rx) = state.memory.upload_admission.open_session("test");
        session.request(1, ObjectKey::Nar("c".repeat(32)), 3, false);
        let Some(Admitted { permit, .. }) = rx.recv().await else {
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
        let (compressed, mut metadata) =
            build_upload_metadata_with_matching_nar_hash_and_size(b"abc");
        if let UploadMetadata::Nar(nar_upload_metadata) = &mut metadata {
            nar_upload_metadata.file_size += 96;
        }
        writer.append(0, &compressed).await.unwrap();
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            handles: SessionHandles::from_state(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: ProjectId::nil(),
            dispatched_job: None,
            key: ObjectKey::Nar("c".repeat(32)),
            segment_cache: None,
            object: CommittedUpload::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
                deriver_decision: Ok(test_build_job_output_deriver_decision()),
            },
            transfer: Transfer::Passthrough(Box::new(writer)),
            metadata,
            server_granted_upload_size_bytes: 3,
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
    async fn a_presigned_nar_that_was_never_uploaded_is_retried() {
        let state = test_state(empty_db());
        let (_session, permit) = granted_permit(&state).await;
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            handles: SessionHandles::from_state(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: ProjectId::nil(),
            dispatched_job: None,
            key: ObjectKey::Nar("c".repeat(32)),
            segment_cache: None,
            object: CommittedUpload::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
                deriver_decision: Ok(test_build_job_output_deriver_decision()),
            },
            transfer: Transfer::PutToTemporaryUploadObject {
                temporary_nar_storage_key: "nar-uploads/missing.nar.zst".into(),
            },
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
            server_granted_upload_size_bytes: 3,
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

    fn build_upload_metadata_with_mismatched_nar_hash_and_size(
        nar: &[u8],
        claimed_nar: &[u8],
    ) -> (Vec<u8>, UploadMetadata) {
        use sha2::Digest as _;
        let compressed = zstd::encode_all(nar, 3).unwrap();
        let metadata = UploadMetadata::Nar(Box::new(NarUploadMetadata {
            file_hash: gradient_storage::file_hash_sri(&compressed),
            file_size: compressed.len() as u64,
            nar_size: claimed_nar.len() as u64,
            nar_hash: format!("sha256:{}", hex::encode(sha2::Sha256::digest(claimed_nar))),
            references: Vec::new(),
            deriver: None,
            ca: None,
            multipart: None,
        }));
        (compressed, metadata)
    }

    #[tokio::test]
    async fn rejects_presigned_put_with_mismatched_nar_hash_and_deletes_temporary_object() {
        let hash = "x".repeat(32);
        let state = test_state(
            mock_db()
                .append_query_results([Vec::<gradient_types::MCachedPath>::new()])
                .into_connection(),
        );
        let (compressed, metadata) =
            build_upload_metadata_with_mismatched_nar_hash_and_size(b"nar X", b"nar Y");
        let key = format!("nar-uploads/{hash}-1.nar.zst");
        let file = put_temporary_upload(&state, &key, &compressed).await;

        let placed = place(
            &state.db(),
            &ObjectKey::Nar(hash.clone()),
            &UploadObject::Nar {
                store_path: format!("/nix/store/{hash}-p"),
            },
            &format!("/nix/store/{hash}-p"),
            Transfer::PutToTemporaryUploadObject {
                temporary_nar_storage_key: key.clone(),
            },
            &metadata,
            3,
        )
        .await;

        assert!(
            matches!(placed, Err(UploadOutcome::Rejected { .. })),
            "Reject a byte sequence of NAR X with the declared hash of NAR Y. {placed:?}"
        );
        assert!(!file.exists(), "Delete temporary uploads");
        assert!(
            state.nar_storage.get(&hash).await.unwrap().is_none(),
            "No values exist under the store path key."
        );
    }

    #[tokio::test]
    async fn rejects_passthrough_nar_with_wrong_hash_and_writes_nothing_to_store_path() {
        let hash = "y".repeat(32);
        let state = test_state(
            mock_db()
                .append_query_results([Vec::<gradient_types::MCachedPath>::new()])
                .into_connection(),
        );
        let (compressed, metadata) =
            build_upload_metadata_with_mismatched_nar_hash_and_size(b"nar X", b"nar Y");
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials.open_writer("peer/y", "y", 0, 0).await.unwrap();
        writer.append(0, &compressed).await.unwrap();

        let placed = place(
            &state.db(),
            &ObjectKey::Nar(hash.clone()),
            &UploadObject::Nar {
                store_path: format!("/nix/store/{hash}-p"),
            },
            &format!("/nix/store/{hash}-p"),
            Transfer::Passthrough(Box::new(writer)),
            &metadata,
            3,
        )
        .await;

        assert!(
            matches!(placed, Err(UploadOutcome::Rejected { .. })),
            "Reject a byte sequence of NAR X with the declared hash of NAR Y. {placed:?}"
        );
        assert!(
            state.nar_storage.get(&hash).await.unwrap().is_none(),
            "No values exist under the store path key."
        );
    }

    fn build_upload_metadata_with_matching_nar_hash_and_size(
        nar: &[u8],
    ) -> (Vec<u8>, UploadMetadata) {
        build_upload_metadata_with_mismatched_nar_hash_and_size(nar, nar)
    }

    async fn put_temporary_upload(
        state: &ServerState,
        key: &str,
        bytes: &[u8],
    ) -> std::path::PathBuf {
        let file = state.nar_storage.local_base().unwrap().join(key);
        tokio::fs::create_dir_all(file.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&file, bytes).await.unwrap();
        file
    }

    #[tokio::test]
    async fn a_presigned_nar_for_a_stored_path_leaves_the_stored_object() {
        let hash = "s".repeat(32);
        let stored = gradient_types::MCachedPath {
            hash: hash.clone(),
            nar_object_storage_key: hash.clone(),
            file_hash: "sha256:stored".into(),
            ..Default::default()
        };
        let state = test_state(
            mock_db()
                .append_query_results([vec![stored]])
                .into_connection(),
        );
        state.nar_storage.put(&hash, b"OLD".to_vec()).await.unwrap();
        let (compressed, metadata) = build_upload_metadata_with_matching_nar_hash_and_size(b"abc");
        let key = format!("nar-uploads/{hash}-1.nar.zst");
        let file = put_temporary_upload(&state, &key, &compressed).await;

        place(
            &state.db(),
            &ObjectKey::Nar(hash.clone()),
            &UploadObject::Nar {
                store_path: format!("/nix/store/{hash}-p"),
            },
            &format!("/nix/store/{hash}-p"),
            Transfer::PutToTemporaryUploadObject {
                temporary_nar_storage_key: key.clone(),
            },
            &metadata,
            3,
        )
        .await
        .unwrap_or_else(|outcome| panic!("{outcome:?}"));

        assert_eq!(state.nar_storage.get(&hash).await.unwrap().unwrap(), b"OLD");
        assert!(!file.exists(), "the temporary upload is deleted");
    }

    #[tokio::test]
    async fn a_presigned_nar_for_a_new_path_is_copied_into_place() {
        let hash = "n".repeat(32);
        let state = test_state(
            mock_db()
                .append_query_results([Vec::<gradient_types::MCachedPath>::new()])
                .into_connection(),
        );
        let (compressed, metadata) = build_upload_metadata_with_matching_nar_hash_and_size(b"abc");
        let key = format!("nar-uploads/{hash}-1.nar.zst");
        let file = put_temporary_upload(&state, &key, &compressed).await;

        place(
            &state.db(),
            &ObjectKey::Nar(hash.clone()),
            &UploadObject::Nar {
                store_path: format!("/nix/store/{hash}-p"),
            },
            &format!("/nix/store/{hash}-p"),
            Transfer::PutToTemporaryUploadObject {
                temporary_nar_storage_key: key.clone(),
            },
            &metadata,
            3,
        )
        .await
        .unwrap_or_else(|outcome| panic!("{outcome:?}"));

        let variant_key = gradient_storage::verify_zstd_nar_stream_sha256_and_sizes_against_claims(
            compressed.as_slice(),
            &gradient_storage::ClientClaimedNarUploadHashesAndSizes {
                file_hash: &gradient_storage::file_hash_sri(&compressed),
                file_size: compressed.len() as u64,
                nar_hash: &gradient_storage::file_hash_sri(b"abc"),
                nar_size: 3,
                claimed_store_path_references: &[],
                uploaded_nar_with_stated_content_address_to_check: None,
            },
        )
        .await
        .unwrap()
        .object_storage_key_for_store_path_hash_and_compressed_file_sha256(&hash);
        assert_eq!(
            state.nar_storage.get(&variant_key).await.unwrap().unwrap(),
            compressed
        );
        assert!(state.nar_storage.get(&hash).await.unwrap().is_none());
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn bytes_written_through_presigned_upload_link_after_verification_are_not_stored_under_final_key()
     {
        let hash = "w".repeat(32);
        let state = test_state(empty_db());
        let (compressed, metadata) = build_upload_metadata_with_matching_nar_hash_and_size(b"abc");
        let UploadMetadata::Nar(meta) = metadata else {
            unreachable!()
        };
        let swapped = zstd::encode_all(&b"evil"[..], 3).unwrap();
        let key = format!("nar-uploads/{hash}-1.nar.zst");
        let file = put_temporary_upload(&state, &key, &compressed).await;

        let store_path = format!("/nix/store/{hash}-swapped");
        let verified = commit_worker_upload_to_store_path(
            &state.db(),
            &hash,
            &store_path,
            &key,
            &meta,
            async { tokio::fs::write(&file, &swapped).await.unwrap() },
        )
        .await
        .unwrap_or_else(|outcome| panic!("{outcome:?}"));

        let variant_key =
            verified.object_storage_key_for_store_path_hash_and_compressed_file_sha256(&hash);
        assert_eq!(verified.file_size(), compressed.len() as u64);
        assert_eq!(
            state.nar_storage.get(&variant_key).await.unwrap().unwrap(),
            compressed,
            "verified bytes at the variant key"
        );
        let mut directories = vec![state.nar_storage.local_base().unwrap().to_path_buf()];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    directories.push(path);
                } else if path != file {
                    assert_ne!(std::fs::read(&path).unwrap(), swapped, "{path:?}");
                }
            }
        }
    }

    #[tokio::test]
    async fn a_passed_through_nar_for_a_stored_path_leaves_the_stored_object() {
        let hash = "s".repeat(32);
        let stored = gradient_types::MCachedPath {
            hash: hash.clone(),
            nar_object_storage_key: hash.clone(),
            file_hash: "sha256:stored".into(),
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
        let (compressed, metadata) = build_upload_metadata_with_matching_nar_hash_and_size(b"abc");
        writer.append(0, &compressed).await.unwrap();

        place(
            &state.db(),
            &ObjectKey::Nar(hash.clone()),
            &UploadObject::Nar {
                store_path: format!("/nix/store/{hash}-p"),
            },
            &format!("/nix/store/{hash}-p"),
            Transfer::Passthrough(Box::new(writer)),
            &metadata,
            3,
        )
        .await
        .unwrap_or_else(|outcome| panic!("the upload settles: {outcome:?}"));

        assert_eq!(state.nar_storage.get(&hash).await.unwrap().unwrap(), b"OLD");
    }

    #[tokio::test]
    async fn second_same_worker_connection_is_not_granted_upload_while_first_commits_and_cannot_open_staging_file()
     {
        let state = test_state(empty_db());
        let hash = "c".repeat(32);
        let (first_connection, mut permit) = granted_permit(&state).await;
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials
            .open_writer(&format!("w1/{hash}"), &hash, 0, 0)
            .await
            .unwrap();
        let (compressed, metadata) = build_upload_metadata_with_matching_nar_hash_and_size(b"abc");
        writer.append(0, &compressed).await.unwrap();
        permit.release_budget();
        drop(first_connection);

        let (second_connection, mut second_connection_upload_admission_grant_receiver) =
            state.memory.upload_admission.open_session("test");
        second_connection.request(7, ObjectKey::Nar(hash.clone()), 3, false);
        assert!(
            second_connection_upload_admission_grant_receiver
                .try_recv()
                .is_err(),
            "second push of the hash can't start before the first upload"
        );

        let Ok(UploadPlacementResult::Nar(verified)) = place(
            &state.db(),
            &ObjectKey::Nar(hash.clone()),
            &UploadObject::Nar {
                store_path: format!("/nix/store/{hash}-p"),
            },
            &format!("/nix/store/{hash}-p"),
            Transfer::Passthrough(Box::new(writer)),
            &metadata,
            3,
        )
        .await
        else {
            panic!("place passthrough NAR")
        };
        let variant_key =
            verified.object_storage_key_for_store_path_hash_and_compressed_file_sha256(&hash);
        assert_eq!(
            state.nar_storage.get(&variant_key).await.unwrap().unwrap(),
            compressed
        );
        assert!(
            second_connection_upload_admission_grant_receiver
                .try_recv()
                .is_err()
        );

        drop(permit);
        assert!(matches!(
            second_connection_upload_admission_grant_receiver.try_recv(),
            Ok(Admitted { id: 7, .. })
        ));
    }

    #[tokio::test]
    async fn second_worker_gets_upload_when_nar_hash_already_stored_and_server_verifies_bytes() {
        let state = test_state(empty_db());
        let hash = "c".repeat(32);
        let store_path = UploadObject::Nar {
            store_path: format!("/nix/store/{hash}-p"),
        };
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let (compressed, metadata) = build_upload_metadata_with_matching_nar_hash_and_size(b"abc");
        let (_first_producer_upload_session, first_permit) = granted_permit(&state).await;
        let (second_producer_upload_session, mut second_producer_upload_grant_receiver) =
            state.memory.upload_admission.open_session("test");
        second_producer_upload_session.request(2, ObjectKey::Nar(hash.clone()), 3, false);
        assert!(
            second_producer_upload_grant_receiver.try_recv().is_err(),
            "Second producers always wait during an upload by the first producer."
        );

        let mut first_producer_partial_file_writer = partials
            .open_writer(&format!("w1/{hash}"), &hash, 0, 0)
            .await
            .unwrap();
        first_producer_partial_file_writer
            .append(0, &compressed)
            .await
            .unwrap();
        let first = place(
            &state.db(),
            &ObjectKey::Nar(hash.clone()),
            &store_path,
            &format!("/nix/store/{hash}-p"),
            Transfer::Passthrough(Box::new(first_producer_partial_file_writer)),
            &metadata,
            3,
        )
        .await;
        assert!(matches!(first, Ok(UploadPlacementResult::Nar(_))));
        drop(first_permit);

        let Ok(Admitted { id: 2, .. }) = second_producer_upload_grant_receiver.try_recv() else {
            panic!("second producer got its upload grant");
        };
        let mut second_producer_partial_file_writer = partials
            .open_writer(&format!("w2/{hash}"), &hash, 0, 0)
            .await
            .unwrap();
        second_producer_partial_file_writer
            .append(0, &compressed)
            .await
            .unwrap();
        let Ok(UploadPlacementResult::Nar(verified)) = place(
            &state.db(),
            &ObjectKey::Nar(hash.clone()),
            &store_path,
            &format!("/nix/store/{hash}-p"),
            Transfer::Passthrough(Box::new(second_producer_partial_file_writer)),
            &metadata,
            3,
        )
        .await
        else {
            panic!("second producer's bytes verified")
        };
        let variant_key =
            verified.object_storage_key_for_store_path_hash_and_compressed_file_sha256(&hash);
        assert_eq!(
            state.nar_storage.get(&variant_key).await.unwrap().unwrap(),
            compressed
        );
    }

    #[tokio::test]
    async fn a_placed_nar_frees_its_budget_but_keeps_its_path_until_the_graph_records_it() {
        let Ok(mut unwired) = Arc::try_unwrap(test_state(
            mock_db()
                .append_query_results([Vec::<gradient_types::MCachedPath>::new()])
                .append_query_results([Vec::<gradient_types::MTeamWorker>::new()])
                .append_query_results([Vec::<gradient_types::MWorkerRegistration>::new()])
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
        let (compressed, metadata) = build_upload_metadata_with_matching_nar_hash_and_size(b"abc");
        writer.append(0, &compressed).await.unwrap();
        let (writer_out, _sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        let commit = run(Commit {
            writer: writer_out,
            handles: SessionHandles::from_state(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: ProjectId::nil(),
            dispatched_job: None,
            key: ObjectKey::Nar("c".repeat(32)),
            segment_cache: None,
            object: CommittedUpload::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
                deriver_decision: Ok(test_build_job_output_deriver_decision()),
            },
            transfer: Transfer::Passthrough(Box::new(writer)),
            metadata,
            server_granted_upload_size_bytes: 3,
            permit,
        });
        tokio::pin!(commit);

        let freed = async {
            while state.memory.upload_admission.in_flight() != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
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
        let (compressed, metadata) =
            build_upload_metadata_with_mismatched_nar_hash_and_size(b"abc", b"xyz");
        partials
            .append("peer/c", "c", 0, &compressed[..2])
            .await
            .unwrap();
        let mut writer = partials.open_writer("peer/c", "c", 2, 0).await.unwrap();
        assert!(writer.resumed());
        writer.append(2, &compressed[2..]).await.unwrap();
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            handles: SessionHandles::from_state(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: ProjectId::nil(),
            dispatched_job: None,
            key: ObjectKey::Nar("c".repeat(32)),
            segment_cache: None,
            object: CommittedUpload::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
                deriver_decision: Ok(test_build_job_output_deriver_decision()),
            },
            transfer: Transfer::Passthrough(Box::new(writer)),
            metadata,
            server_granted_upload_size_bytes: 3,
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
            "",
            Transfer::Passthrough(Box::new(writer)),
            &UploadMetadata::ExecOutput {
                size_bytes: body.len() as u64,
            },
            body.len() as u64,
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

    async fn commit_exec_output(
        state: &Arc<ServerState>,
        job: ExecJobId,
        body: &[u8],
        server_granted_upload_size_bytes: u64,
    ) -> UploadOutcome {
        let (_session, permit) = granted_permit(state).await;
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials.open_writer("peer/o", "o", 0, 0).await.unwrap();
        writer.append(0, body).await.unwrap();
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            handles: SessionHandles::from_state(state),
            peer_id: "w1".into(),
            request_id: 1,
            project: ProjectId::nil(),
            dispatched_job: None,
            key: ObjectKey::Rest(gradient_storage::exec_output_key(
                job.into_inner(),
                "model/w.bin",
            )),
            segment_cache: None,
            object: CommittedUpload::Other(UploadObject::ExecOutput {
                job: job.to_string(),
                path: "model/w.bin".into(),
            }),
            transfer: Transfer::Passthrough(Box::new(writer)),
            metadata: UploadMetadata::ExecOutput {
                size_bytes: body.len() as u64,
            },
            server_granted_upload_size_bytes,
            permit,
        })
        .await;

        assert_eq!(state.memory.upload_admission.in_flight(), 0);
        match gradient_wire::codec::from_bytes::<ServerMessage>(
            sent.try_recv().unwrap(),
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .unwrap()
        {
            ServerMessage::UploadCommitted {
                request_id: 1,
                outcome,
            } => outcome,
            other => panic!("expected the commit outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn commit_rejects_exec_job_output_file_staged_length_larger_than_granted_size_and_stores_nothing()
     {
        let state = test_state(empty_db());
        let job = ExecJobId::now_v7();
        let body = [7u8; 64];

        let outcome = commit_exec_output(&state, job, &body, 63).await;

        let UploadOutcome::Rejected { reason } = outcome else {
            panic!("expected a rejection, got {outcome:?}")
        };
        assert!(reason.contains("63"), "{reason}");
        assert!(
            state
                .nar_storage
                .get_exec_output(job.into_inner(), "model/w.bin", body.len())
                .await
                .unwrap()
                .is_none(),
            "nothing is adopted"
        );
    }

    #[tokio::test]
    async fn exec_job_output_file_staged_length_equal_to_granted_size_is_stored_at_commit() {
        let state = test_state(
            mock_db()
                .append_exec_results([gradient_fixtures::mock::exec_ok()])
                .into_connection(),
        );
        let job = ExecJobId::now_v7();
        let body = [7u8; 64];

        let outcome = commit_exec_output(&state, job, &body, 64).await;

        assert!(matches!(outcome, UploadOutcome::Ok), "{outcome:?}");
        assert!(
            state
                .nar_storage
                .get_exec_output(job.into_inner(), "model/w.bin", body.len())
                .await
                .unwrap()
                .is_some(),
            "the output is adopted"
        );
    }

    async fn stage_bytes_and_place_eval_cache_segment_with_upload_size(
        state: &Arc<ServerState>,
        body: &[u8],
        server_granted_upload_size_bytes: u64,
    ) -> (Result<UploadPlacementResult, UploadOutcome>, String) {
        let key = format!("eval-cache/{}", uuid::Uuid::now_v7());
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials.open_writer("peer/s", "s", 0, 0).await.unwrap();
        writer.append(0, body).await.unwrap();

        let placed = place(
            &state.db(),
            &ObjectKey::EvalCache(key.clone()),
            &UploadObject::EvalCacheSegment {
                fingerprint: "f".into(),
            },
            "",
            Transfer::Passthrough(Box::new(writer)),
            &UploadMetadata::EvalCache {
                size_bytes: body.len() as u64,
            },
            server_granted_upload_size_bytes,
        )
        .await;
        (placed, key)
    }

    #[tokio::test]
    async fn commit_rejects_eval_cache_segment_staged_length_larger_than_granted_size_and_stores_nothing()
     {
        let state = test_state(empty_db());
        let body = [7u8; 64];

        let (placed, key) =
            stage_bytes_and_place_eval_cache_segment_with_upload_size(&state, &body, 63).await;

        let Err(UploadOutcome::Rejected { reason }) = placed else {
            panic!("expected a rejection, got {placed:?}")
        };
        assert!(reason.contains("allowed size is 63"), "{reason}");
        assert!(
            state
                .nar_storage
                .get_eval_cache(&key)
                .await
                .unwrap()
                .is_none(),
            "nothing is stored"
        );
    }

    #[tokio::test]
    async fn eval_cache_segment_staged_length_equal_to_granted_upload_size_is_stored_at_commit() {
        let state = test_state(empty_db());
        let body = [7u8; 64];

        let (placed, key) =
            stage_bytes_and_place_eval_cache_segment_with_upload_size(&state, &body, 64).await;

        assert!(
            matches!(placed, Ok(UploadPlacementResult::EvalCacheSegment)),
            "{placed:?}"
        );
        assert_eq!(
            state.nar_storage.get_eval_cache(&key).await.unwrap(),
            Some(body.to_vec())
        );
    }

    const OTHER_ORIGINAL_HASH: &str = "abcdfghijklmnpqrsvwxyz0123456789";

    fn test_build_job_output_deriver_decision() -> UploadedStorePathDeriverChoice {
        UploadedStorePathDeriverChoice::DispatchedBuildJobOutputDerivation {
            dispatched_deriver_path: format!("/nix/store/{OTHER_ORIGINAL_HASH}-job.drv"),
        }
    }

    #[tokio::test]
    async fn commit_uses_dispatched_job_derivation_when_worker_reports_another_deriver() {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let store_path = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");
        let stated_deriver_store_path_not_build_job_derivation =
            format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-other.drv");
        let (compressed, mut metadata) = build_upload_metadata_with_matching_nar_hash_and_size(
            &single_file_nar(b"hello", false),
        );
        if let UploadMetadata::Nar(meta) = &mut metadata {
            meta.deriver = Some(stated_deriver_store_path_not_build_job_derivation);
        }
        let key = format!("nar-uploads/{EXAMPLE_ORIGINAL_HASH}-1.nar.zst");
        put_temporary_upload(&state, &key, &compressed).await;
        let dispatched_build_job_output_derivation_decision =
            test_build_job_output_deriver_decision();
        let UploadedStorePathDeriverChoice::DispatchedBuildJobOutputDerivation {
            dispatched_deriver_path: dispatched_build_job_drv_store_path,
        } = dispatched_build_job_output_derivation_decision.clone()
        else {
            unreachable!()
        };

        let placed = map_upload_metadata_to_stored_form_and_place_upload(
            &state.db(),
            ProjectId::nil(),
            &ObjectKey::Nar(EXAMPLE_ORIGINAL_HASH.to_owned()),
            &CommittedUpload::Nar {
                store_path,
                deriver_decision: Ok(dispatched_build_job_output_derivation_decision),
            },
            Transfer::PutToTemporaryUploadObject {
                temporary_nar_storage_key: key,
            },
            &metadata,
            3,
        )
        .await;

        let Ok((Some(stored), UploadPlacementResult::Nar(_))) = placed else {
            panic!("{placed:?}")
        };
        assert_eq!(
            stored.deriver(),
            Some(dispatched_build_job_drv_store_path.as_str())
        );
    }

    #[tokio::test]
    async fn test_store_path_not_listed_as_derivation_output_does_not_store_deriver_even_if_worker_reports_one()
     {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let store_path =
            include_str!("../../../../gradient-ssh/testdata/input-addressed-top.drv.path").trim();
        let input =
            include_str!("../../../../gradient-ssh/testdata/input-addressed-dep.drv.path").trim();
        let hash = super::super::nar_key(store_path).unwrap();
        let (compressed, mut metadata) = compress_nar_with_zstd_and_build_upload_metadata_with_matching_nar_hash_size_and_store_path_references(
            &single_file_nar(
                include_bytes!("../../../../gradient-ssh/testdata/input-addressed-top.drv"),
                false,
            ),
            vec![input.to_owned()],
        );
        if let UploadMetadata::Nar(meta) = &mut metadata {
            meta.deriver = Some(input.to_owned());
        }
        let key = format!("nar-uploads/{hash}-1.nar.zst");
        put_temporary_upload(&state, &key, &compressed).await;

        let placed = map_upload_metadata_to_stored_form_and_place_upload(
            &state.db(),
            ProjectId::nil(),
            &ObjectKey::Nar(hash),
            &CommittedUpload::Nar {
                store_path: store_path.to_owned(),
                deriver_decision: Ok(UploadedStorePathDeriverChoice::NoRecordedDeriver),
            },
            Transfer::PutToTemporaryUploadObject {
                temporary_nar_storage_key: key,
            },
            &metadata,
            3,
        )
        .await;

        let Ok((Some(stored), UploadPlacementResult::Nar(_))) = placed else {
            panic!("{placed:?}")
        };
        assert_eq!(stored.deriver(), None);
    }

    #[tokio::test]
    async fn test_commit_of_withdrawn_job_upload_is_refused_and_temporary_upload_object_deleted() {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let store_path = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");
        let (compressed, metadata) = build_upload_metadata_with_matching_nar_hash_and_size(
            &single_file_nar(b"hello", false),
        );
        let key = format!("nar-uploads/{EXAMPLE_ORIGINAL_HASH}-1.nar.zst");
        let temporary_nar_upload_file_path = put_temporary_upload(&state, &key, &compressed).await;

        let placed = map_upload_metadata_to_stored_form_and_place_upload(
            &state.db(),
            ProjectId::nil(),
            &ObjectKey::Nar(EXAMPLE_ORIGINAL_HASH.to_owned()),
            &CommittedUpload::Nar {
                store_path,
                deriver_decision: Err("This session does not run job j.".to_owned()),
            },
            Transfer::PutToTemporaryUploadObject {
                temporary_nar_storage_key: key,
            },
            &metadata,
            3,
        )
        .await;

        let Err(UploadOutcome::Rejected { reason }) = placed else {
            panic!("{placed:?}")
        };
        assert_eq!(reason, "This session does not run job j.");
        assert!(!temporary_nar_upload_file_path.exists());
    }

    fn compress_nar_with_zstd_and_build_upload_metadata_with_matching_nar_hash_size_and_store_path_references(
        nar: &[u8],
        references: Vec<String>,
    ) -> (Vec<u8>, UploadMetadata) {
        let (compressed, mut metadata) = build_upload_metadata_with_matching_nar_hash_and_size(nar);
        if let UploadMetadata::Nar(meta) = &mut metadata {
            meta.references = references;
        }
        (compressed, metadata)
    }

    async fn run_map_and_place_on_staged_zstd_nar_upload_with_references(
        state: &ServerState,
        store_path: &str,
        nar: &[u8],
        references: Vec<String>,
        content_address: Option<String>,
    ) -> Result<
        (
            Option<CheckedUploadedStorePathReferencesDeriverAndContentAddress>,
            UploadPlacementResult,
        ),
        UploadOutcome,
    > {
        let hash = super::super::nar_key(store_path).unwrap();
        let (compressed, mut metadata) = compress_nar_with_zstd_and_build_upload_metadata_with_matching_nar_hash_size_and_store_path_references(nar, references);
        if let UploadMetadata::Nar(meta) = &mut metadata {
            meta.ca = content_address;
        }
        let key = format!("nar-uploads/{hash}-1.nar.zst");
        put_temporary_upload(state, &key, &compressed).await;
        map_upload_metadata_to_stored_form_and_place_upload(
            &state.db(),
            ProjectId::nil(),
            &ObjectKey::Nar(hash),
            &CommittedUpload::Nar {
                store_path: store_path.to_owned(),
                deriver_decision: Ok(test_build_job_output_deriver_decision()),
            },
            Transfer::PutToTemporaryUploadObject {
                temporary_nar_storage_key: key,
            },
            &metadata,
            3,
        )
        .await
    }

    fn build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
        mapping: Arc<dyn gradient_types::StorePathMapping>,
    ) -> Arc<ServerState> {
        gradient_test_support::state::TestState::new()
            .worker_db(
                mock_db()
                    .append_query_results([Vec::<gradient_types::MCachedPath>::new()])
                    .into_connection(),
            )
            .store_path_mapping(mapping)
            .build()
    }

    #[tokio::test]
    async fn rejects_commit_with_nar_metadata_reference_absent_from_nar_and_writes_nothing() {
        let state = test_state(empty_db());
        let (_session, permit) = granted_permit(&state).await;
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials.open_writer("peer/c", "c", 0, 0).await.unwrap();
        let missing = format!("/nix/store/{OTHER_ORIGINAL_HASH}-bash");
        let (compressed, metadata) =
            compress_nar_with_zstd_and_build_upload_metadata_with_matching_nar_hash_size_and_store_path_references(&single_file_nar(b"no references", false), vec![missing.clone()]);
        writer.append(0, &compressed).await.unwrap();
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            handles: SessionHandles::from_state(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: ProjectId::nil(),
            dispatched_job: None,
            key: ObjectKey::Nar("c".repeat(32)),
            segment_cache: None,
            object: CommittedUpload::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
                deriver_decision: Ok(test_build_job_output_deriver_decision()),
            },
            transfer: Transfer::Passthrough(Box::new(writer)),
            metadata,
            server_granted_upload_size_bytes: 3,
            permit,
        })
        .await;

        let msg = gradient_wire::codec::from_bytes::<ServerMessage>(
            sent.try_recv().unwrap(),
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .unwrap();
        let ServerMessage::UploadCommitted {
            outcome: UploadOutcome::Rejected { reason },
            ..
        } = msg
        else {
            panic!("{msg:?}")
        };
        assert!(reason.contains(&missing), "{reason}");
        assert!(
            state
                .nar_storage
                .get(&"c".repeat(32))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(state.memory.upload_admission.in_flight(), 0);
    }

    #[tokio::test]
    async fn rejects_presigned_put_with_missing_nar_reference_and_deletes_temporary_object() {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let store_path = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");
        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &store_path,
            &single_file_nar(b"no references", false),
            vec![format!("/nix/store/{OTHER_ORIGINAL_HASH}-bash")],
            None,
        )
        .await;

        assert!(
            matches!(placed, Err(UploadOutcome::Rejected { .. })),
            "{placed:?}"
        );
        let temporary = state
            .nar_storage
            .local_base()
            .unwrap()
            .join(format!("nar-uploads/{EXAMPLE_ORIGINAL_HASH}-1.nar.zst"));
        assert!(!temporary.exists());
    }

    #[tokio::test]
    async fn worker_nar_upload_commit_with_bad_reference_deriver_or_content_address_is_rejected_and_stores_no_nar()
     {
        let store_path = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");
        let drv = format!("{EXAMPLE_ORIGINAL_HASH}-hello.drv");
        let ca = "fixed:r:sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf";
        let cases = [
            (
                "reference with a line feed",
                vec![format!("{store_path}\nInjected: 1")],
                None,
                None,
            ),
            (
                "reference that is no store path",
                vec!["hello".into()],
                None,
                None,
            ),
            (
                "deriver with a line feed",
                vec![],
                Some(format!("{drv}\nInjected: 1")),
                None,
            ),
            (
                "deriver that is no .drv",
                vec![],
                Some(store_path.clone()),
                None,
            ),
            (
                "content address with a line feed",
                vec![],
                None,
                Some(format!("{ca}\nInjected: 1")),
            ),
            (
                "content address of an unknown form",
                vec![],
                None,
                Some(ca.replace("fixed:r", "nix")),
            ),
        ];
        let mut accepted = Vec::new();
        for (case, references, deriver, content_address) in cases {
            let state =
                build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                    Arc::new(gradient_entity::IdentityMapping),
                );
            let (compressed, mut metadata) =
                compress_nar_with_zstd_and_build_upload_metadata_with_matching_nar_hash_size_and_store_path_references(&single_file_nar(store_path.as_bytes(), false), references);
            if let UploadMetadata::Nar(meta) = &mut metadata {
                meta.deriver = deriver;
                meta.ca = content_address;
            }
            let key = format!("nar-uploads/{EXAMPLE_ORIGINAL_HASH}-1.nar.zst");
            put_temporary_upload(&state, &key, &compressed).await;
            let temporary = state.nar_storage.local_base().unwrap().join(&key);
            let placed = map_upload_metadata_to_stored_form_and_place_upload(
                &state.db(),
                ProjectId::nil(),
                &ObjectKey::Nar(EXAMPLE_ORIGINAL_HASH.to_owned()),
                &CommittedUpload::Nar {
                    store_path: store_path.clone(),
                    deriver_decision: Ok(test_build_job_output_deriver_decision()),
                },
                Transfer::PutToTemporaryUploadObject {
                    temporary_nar_storage_key: key,
                },
                &metadata,
                3,
            )
            .await;
            match placed {
                Err(UploadOutcome::Rejected { .. }) => {
                    assert!(
                        state
                            .nar_storage
                            .get(EXAMPLE_ORIGINAL_HASH)
                            .await
                            .unwrap()
                            .is_none(),
                        "{case}"
                    );
                    assert!(!temporary.exists(), "{case}");
                }
                other => accepted.push(format!("{case}: {other:?}")),
            }
        }
        assert_eq!(accepted, Vec::<String>::new());
    }

    #[tokio::test]
    async fn worker_nar_upload_for_project_without_store_path_mapping_is_rejected_and_temporary_upload_is_deleted_and_no_final_nar_is_stored()
     {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::UnresolvableProjectMapping),
            );
        let store_path = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");
        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &store_path,
            &single_file_nar(b"no references", false),
            vec![],
            None,
        )
        .await;

        assert!(
            matches!(placed, Err(UploadOutcome::Rejected { .. })),
            "{placed:?}"
        );
        let temporary = state
            .nar_storage
            .local_base()
            .unwrap()
            .join(format!("nar-uploads/{EXAMPLE_ORIGINAL_HASH}-1.nar.zst"));
        assert!(!temporary.exists());
        assert!(
            state
                .nar_storage
                .get(EXAMPLE_ORIGINAL_HASH)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn worker_nar_upload_with_all_claimed_references_present_is_stored() {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let store_path = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");
        let other = format!("/nix/store/{OTHER_ORIGINAL_HASH}-bash");
        let nar = single_file_nar(format!("#!{other}/bin/sh\n{store_path}").as_bytes(), true);

        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &store_path,
            &nar,
            vec![other, store_path.clone()],
            None,
        )
        .await;

        assert!(
            matches!(placed, Ok((_, UploadPlacementResult::Nar(_)))),
            "{placed:?}"
        );
    }

    #[tokio::test]
    async fn worker_nar_upload_is_stored_when_store_path_hash_mapping_changes_hashes() {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::RotatedHashMapping),
            );
        let store_path = format!("/nix/store/{OTHER_ORIGINAL_HASH}-hello");
        let dependency = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-bash");
        let nar = single_file_nar(format!("#!{dependency}/bin/sh").as_bytes(), true);

        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &store_path,
            &nar,
            vec![dependency],
            None,
        )
        .await;

        let Ok((Some(stored), UploadPlacementResult::Nar(_))) = placed else {
            panic!("{placed:?}")
        };
        assert_eq!(
            stored.references(),
            [format!("/nix/store/{EXAMPLE_STORED_HASH}-bash")]
        );
    }

    #[tokio::test]
    async fn uploading_real_derivation_with_its_references_is_accepted() {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let derivation =
            include_bytes!("../../../../gradient-ssh/testdata/input-addressed-top.drv");
        let store_path =
            include_str!("../../../../gradient-ssh/testdata/input-addressed-top.drv.path");
        let input = include_str!("../../../../gradient-ssh/testdata/input-addressed-dep.drv.path");

        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            store_path.trim(),
            &single_file_nar(derivation, false),
            vec![input.trim().to_owned()],
            None,
        )
        .await;

        assert!(
            matches!(placed, Ok((_, UploadPlacementResult::Nar(_)))),
            "{placed:?}"
        );
    }

    async fn assert_worker_upload_rejected_and_objects_absent(
        state: &ServerState,
        store_path: &str,
        placed: Result<
            (
                Option<CheckedUploadedStorePathReferencesDeriverAndContentAddress>,
                UploadPlacementResult,
            ),
            UploadOutcome,
        >,
    ) {
        let hash = super::super::nar_key(store_path).unwrap();
        assert!(
            matches!(placed, Err(UploadOutcome::Rejected { .. })),
            "{placed:?}"
        );
        let temporary = state
            .nar_storage
            .local_base()
            .unwrap()
            .join(format!("nar-uploads/{hash}-1.nar.zst"));
        assert!(!temporary.exists());
        assert!(state.nar_storage.get(&hash).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn evaluation_job_upload_of_input_addressed_path_without_content_address_is_stored_with_worker_deriver()
     {
        let placed = stage_input_addressed_nar_upload_with_deriver_and_run_commit_metadata_and_placement_with_deriver_decision(
            Ok(UploadedStorePathDeriverChoice::UseWorkerStatedDeriver),
        )
        .await;

        let Ok((Some(stored), UploadPlacementResult::Nar(_))) = placed else {
            panic!("{placed:?}")
        };
        assert_eq!(
            stored.deriver(),
            Some(worker_stated_deriver_path().as_str())
        );
    }

    #[tokio::test]
    async fn build_job_uploaded_input_addressed_path_not_listed_output_is_stored_without_deriver() {
        let placed = stage_input_addressed_nar_upload_with_deriver_and_run_commit_metadata_and_placement_with_deriver_decision(
            Ok(UploadedStorePathDeriverChoice::NoRecordedDeriver),
        )
        .await;

        let Ok((Some(stored), UploadPlacementResult::Nar(_))) = placed else {
            panic!("{placed:?}")
        };
        assert_eq!(stored.deriver(), None);
    }

    fn worker_stated_deriver_path() -> String {
        format!("/nix/store/{}-p.drv", "d".repeat(32))
    }

    async fn stage_input_addressed_nar_upload_with_deriver_and_run_commit_metadata_and_placement_with_deriver_decision(
        deriver_decision: Result<UploadedStorePathDeriverChoice, String>,
    ) -> Result<
        (
            Option<CheckedUploadedStorePathReferencesDeriverAndContentAddress>,
            UploadPlacementResult,
        ),
        UploadOutcome,
    > {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let store_path = format!("/nix/store/{}-p", "c".repeat(32));
        let hash = super::super::nar_key(&store_path).unwrap();
        let (compressed, mut metadata) = build_upload_metadata_with_matching_nar_hash_and_size(
            &single_file_nar(b"input addressed", false),
        );
        if let UploadMetadata::Nar(meta) = &mut metadata {
            meta.deriver = Some(worker_stated_deriver_path());
        }
        let key = format!("nar-uploads/{hash}-1.nar.zst");
        put_temporary_upload(&state, &key, &compressed).await;

        map_upload_metadata_to_stored_form_and_place_upload(
            &state.db(),
            ProjectId::nil(),
            &ObjectKey::Nar(hash),
            &CommittedUpload::Nar {
                store_path,
                deriver_decision,
            },
            Transfer::PutToTemporaryUploadObject {
                temporary_nar_storage_key: key,
            },
            &metadata,
            3,
        )
        .await
    }

    #[tokio::test]
    async fn test_accept_derivation_upload_with_mapped_store_path_and_original_references() {
        use gradient_types::StorePathMapping as _;
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::RotatedHashMapping),
            );
        let original =
            include_str!("../../../../gradient-ssh/testdata/input-addressed-top.drv.path")
                .trim()
                .trim_start_matches("/nix/store/");
        let input = include_str!("../../../../gradient-ssh/testdata/input-addressed-dep.drv.path");
        let stored = gradient_entity::RotatedHashMapping.inbound(
            gradient_types::MappingContext::Project(ProjectId::nil()),
            gradient_types::StoreHash::parse(&original[..32]).unwrap(),
        );

        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &format!("/nix/store/{stored}{}", &original[32..]),
            &single_file_nar(
                include_bytes!("../../../../gradient-ssh/testdata/input-addressed-top.drv"),
                false,
            ),
            vec![input.trim().to_owned()],
            None,
        )
        .await;

        assert!(
            matches!(placed, Ok((_, UploadPlacementResult::Nar(_)))),
            "{placed:?}"
        );
    }

    fn derivation_bytes_with_changed_system() -> (Vec<u8>, String, String) {
        let derivation = include_str!("../../../../gradient-ssh/testdata/input-addressed-top.drv");
        let store_path =
            include_str!("../../../../gradient-ssh/testdata/input-addressed-top.drv.path");
        let input = include_str!("../../../../gradient-ssh/testdata/input-addressed-dep.drv.path");
        (
            derivation
                .replace("x86_64-linux", "aarch64-linux")
                .into_bytes(),
            store_path.trim().to_owned(),
            input.trim().to_owned(),
        )
    }

    #[tokio::test]
    async fn test_worker_rejects_drv_text_changed_after_path_with_original_content_address_and_stores_no_nar()
     {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let (changed, store_path, input) = derivation_bytes_with_changed_system();
        let original = compute_derivation_store_path_and_text_content_address(
            "top.drv",
            include_bytes!("../../../../gradient-ssh/testdata/input-addressed-top.drv"),
            std::slice::from_ref(&input),
        );
        assert_eq!(original.store_path, store_path);

        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &store_path,
            &single_file_nar(&changed, false),
            vec![input],
            Some(original.content_address),
        )
        .await;

        assert_worker_upload_rejected_and_objects_absent(&state, &store_path, placed).await;
    }

    #[tokio::test]
    async fn worker_upload_drv_without_content_address_and_path_not_matching_text_is_rejected() {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let (changed, store_path, input) = derivation_bytes_with_changed_system();

        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &store_path,
            &single_file_nar(&changed, false),
            vec![input],
            None,
        )
        .await;

        assert_worker_upload_rejected_and_objects_absent(&state, &store_path, placed).await;
    }

    #[tokio::test]
    async fn worker_upload_fixed_output_with_mismatched_content_hash_is_rejected_and_stores_no_nar()
    {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let claimed = flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes(
            "source.tar.gz",
            b"the tarball the hash was taken from",
        );

        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &claimed.store_path,
            &single_file_nar(b"other tarball bytes", false),
            vec![],
            Some(claimed.content_address),
        )
        .await;

        assert_worker_upload_rejected_and_objects_absent(&state, &claimed.store_path, placed).await;
    }

    #[tokio::test]
    async fn worker_upload_of_fixed_output_file_with_correct_content_address_is_stored() {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::IdentityMapping),
            );
        let contents = b"tarball bytes";
        let fixed_output =
            flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes(
                "source.tar.gz",
                contents,
            );

        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &fixed_output.store_path,
            &single_file_nar(contents, false),
            vec![],
            Some(fixed_output.content_address),
        )
        .await;

        assert!(
            matches!(placed, Ok((_, UploadPlacementResult::Nar(_)))),
            "{placed:?}"
        );
    }

    fn rotate_store_path_hash_characters_by_one(original: &str) -> String {
        use gradient_types::StorePathMapping as _;
        let original = original.trim_start_matches("/nix/store/");
        let stored = gradient_entity::RotatedHashMapping.inbound(
            gradient_types::MappingContext::Project(ProjectId::nil()),
            gradient_types::StoreHash::parse(&original[..32]).unwrap(),
        );
        format!("/nix/store/{stored}{}", &original[32..])
    }

    #[tokio::test]
    async fn worker_upload_fixed_output_file_with_matching_content_address_hash_is_stored_under_rotated_hash_mapping()
     {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::RotatedHashMapping),
            );
        let contents = b"tarball bytes";
        let fixed_output =
            flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes(
                "source.tar.gz",
                contents,
            );

        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &rotate_store_path_hash_characters_by_one(&fixed_output.store_path),
            &single_file_nar(contents, false),
            vec![],
            Some(fixed_output.content_address),
        )
        .await;

        assert!(
            matches!(placed, Ok((_, UploadPlacementResult::Nar(_)))),
            "{placed:?}"
        );
    }

    #[tokio::test]
    async fn worker_upload_rejects_changed_derivation_text_with_original_content_address_under_rotated_hash_mapping_and_leaves_no_nar_objects()
     {
        let state =
            build_test_server_state_with_one_empty_cached_path_lookup_and_store_path_mapping(
                Arc::new(gradient_entity::RotatedHashMapping),
            );
        let (changed, store_path, input) = derivation_bytes_with_changed_system();
        let original = compute_derivation_store_path_and_text_content_address(
            "top.drv",
            include_bytes!("../../../../gradient-ssh/testdata/input-addressed-top.drv"),
            std::slice::from_ref(&input),
        );
        let stored = rotate_store_path_hash_characters_by_one(&store_path);

        let placed = run_map_and_place_on_staged_zstd_nar_upload_with_references(
            &state,
            &stored,
            &single_file_nar(&changed, false),
            vec![input],
            Some(original.content_address),
        )
        .await;

        assert_worker_upload_rejected_and_objects_absent(&state, &stored, placed).await;
    }
}
