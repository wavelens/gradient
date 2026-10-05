/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use futures::future::BoxFuture;
use gradient_core::ServerState;
use gradient_storage::admission::{ObjectKey, UploadPermit};
use gradient_types::ids::ProjectId;
use gradient_wire::messages::ServerMessage;
use gradient_wire::types::{NarUploadMetadata, UploadMetadata, UploadObject, UploadOutcome};
use tracing::warn;

use super::super::nar::{NarUploadRecord, mark_nar_stored, record_nar_push_metric};
use super::super::socket::{ProtoWriter, send_server_msg};
use super::table::Transfer;
use crate::import::{WriteNeeded, nar_write_needed};

pub(super) struct Commit {
    pub writer: ProtoWriter,
    pub state: Arc<ServerState>,
    pub peer_id: String,
    pub request_id: u64,
    /// This is resolved off the session's reader loop. The scheduler is answering from a mailbox
    /// that a claim in flight can hold for its lock waits.
    pub project: BoxFuture<'static, Option<ProjectId>>,
    pub object: UploadObject,
    pub transfer: Transfer,
    pub metadata: UploadMetadata,
    pub permit: UploadPermit,
}

/// The permit is bounding bytes in flight and is returned once the object is stored. The graph is
/// recording it afterwards without holding the next upload.
pub(super) async fn run(c: Commit) {
    let outcome = match place(&c.state, &c.object, c.transfer, &c.metadata).await {
        Ok(()) => {
            c.permit.committed();
            let project_id = c.project.await;
            record(&c.state, project_id, &c.object, &c.metadata).await
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

async fn place(
    state: &Arc<ServerState>,
    object: &UploadObject,
    transfer: Transfer,
    metadata: &UploadMetadata,
) -> Result<(), UploadOutcome> {
    match (object, metadata) {
        (UploadObject::Nar { store_path }, UploadMetadata::Nar(meta)) => {
            let Some(ObjectKey::Nar(hash)) = super::object_key(object) else {
                return Err(rejected(format!("malformed store path {store_path}")));
            };
            match transfer {
                Transfer::Passthrough(writer) => {
                    place_passed_through(state, &hash, *writer, meta).await
                }
                Transfer::Put | Transfer::Multipart { .. } => {
                    place_presigned(state, &hash, meta).await
                }
            }
        }
        (UploadObject::EvalCache { fingerprint }, UploadMetadata::EvalCache { size_bytes }) => {
            place_eval_cache(state, fingerprint, transfer, *size_bytes).await
        }
        _ => Err(rejected("metadata does not match the upload object".into())),
    }
}

async fn record(
    state: &Arc<ServerState>,
    project_id: Option<ProjectId>,
    object: &UploadObject,
    metadata: &UploadMetadata,
) -> UploadOutcome {
    match (object, metadata) {
        (UploadObject::Nar { store_path }, UploadMetadata::Nar(meta)) => {
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
            if let Err(e) = mark_nar_stored(state, project_id, store_path, &record).await {
                return retry(format!("recording {store_path} in the cache index: {e:#}"));
            }
            let _ = record_nar_push_metric(state, project_id, meta.file_size as i64).await;
        }
        (UploadObject::EvalCache { fingerprint }, UploadMetadata::EvalCache { size_bytes }) => {
            super::super::eval_cache::record_eval_cache(state, fingerprint, *size_bytes).await;
        }
        _ => {}
    }
    UploadOutcome::Ok
}

async fn place_passed_through(
    state: &ServerState,
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

    let needed =
        nar_write_needed(&state.worker_db, &state.nar_storage, hash, &meta.file_hash).await;
    if !matches!(needed, Ok(WriteNeeded::Write)) {
        let _ = tokio::fs::remove_file(&staged.path).await;
        needed.map_err(retry)?;
        return Ok(());
    }

    state
        .nar_storage
        .adopt_file(hash, &staged.path)
        .await
        .map_err(retry)?;
    match staged.bytes {
        Some(bytes) => state.nar_storage.hot().insert(hash, bytes),
        None => state.nar_storage.hot().invalidate(hash),
    }
    Ok(())
}

async fn place_presigned(
    state: &ServerState,
    hash: &str,
    meta: &NarUploadMetadata,
) -> Result<(), UploadOutcome> {
    if let Some(receipt) = &meta.multipart
        && let Err(e) = state.nar_storage.complete_multipart(hash, receipt).await
    {
        state
            .nar_storage
            .abort_multipart(hash, &receipt.upload_id)
            .await;
        return Err(retry(format!("completing the multipart upload: {e:#}")));
    }
    state
        .nar_storage
        .verify(
            hash,
            &meta.file_hash,
            meta.file_size,
            state.config.nar.verify_digest,
        )
        .await
        .map_err(retry)?;
    state.nar_storage.hot().invalidate(hash);
    Ok(())
}

async fn place_eval_cache(
    state: &Arc<ServerState>,
    fingerprint: &str,
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
            state
                .nar_storage
                .put_eval_cache(fingerprint, bytes)
                .await
                .map_err(retry)
        }
        Transfer::Put | Transfer::Multipart { .. } => state
            .nar_storage
            .verify_eval_cache(fingerprint, size_bytes)
            .await
            .map_err(retry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_storage::admission::{Admitted, ObjectKey};
    use gradient_test_support::state::test_state;
    use sea_orm::{DatabaseBackend, MockDatabase};

    async fn granted_permit(
        state: &Arc<ServerState>,
    ) -> (
        gradient_storage::admission::AdmissionSession,
        gradient_storage::admission::UploadPermit,
    ) {
        let (session, mut rx) = state.upload_admission.open_session("test");
        session.request(1, ObjectKey::Nar("c".repeat(32)), 3, false);
        let Some(Admitted::Granted { permit, .. }) = rx.recv().await else {
            panic!("granted")
        };
        (session, permit)
    }

    #[tokio::test]
    async fn a_passed_through_nar_with_the_wrong_size_is_rejected_and_frees_its_permit() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let (_session, permit) = granted_permit(&state).await;
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials.open_writer("peer/c", "c", 0, 0).await.unwrap();
        writer.append(0, b"abc").await.unwrap();
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            state: Arc::clone(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: Box::pin(std::future::ready(None)),
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
        assert_eq!(state.upload_admission.in_flight(), 0);
    }

    #[tokio::test]
    async fn a_presigned_nar_that_is_not_in_storage_is_retried() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let (_session, permit) = granted_permit(&state).await;
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            state: Arc::clone(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: Box::pin(std::future::ready(None)),
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
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![stored]])
                .into_connection(),
        );
        state.nar_storage.put(&hash, b"OLD".to_vec()).await.unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let partials = gradient_storage::PartialStore::new(dir.path()).unwrap();
        let mut writer = partials.open_writer("peer/s", "s", 0, 0).await.unwrap();
        writer.append(0, b"abc").await.unwrap();

        place(
            &state,
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
    async fn a_placed_nar_frees_its_permit_before_the_graph_records_it() {
        let Ok(mut unwired) = Arc::try_unwrap(test_state(
            MockDatabase::new(DatabaseBackend::Postgres)
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
            state: Arc::clone(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: Box::pin(std::future::ready(None)),
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
            while state.upload_admission.in_flight() != 0 {
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
        .expect("the permit is back once the bytes are stored");
    }

    #[tokio::test]
    async fn a_resumed_passthrough_that_fails_its_hash_is_retried_from_scratch() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
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
            state: Arc::clone(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project: Box::pin(std::future::ready(None)),
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
}
