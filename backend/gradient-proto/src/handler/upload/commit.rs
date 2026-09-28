/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_core::ServerState;
use gradient_storage::admission::{ObjectKey, UploadPermit};
use gradient_types::ids::ProjectId;
use gradient_wire::messages::ServerMessage;
use gradient_wire::types::{NarUploadMetadata, UploadMetadata, UploadObject, UploadOutcome};
use tracing::warn;

use super::super::nar::{NarUploadRecord, mark_nar_stored, record_nar_push_metric};
use super::super::socket::{ProtoWriter, send_server_msg};
use super::table::Transfer;

pub(super) struct Commit {
    pub writer: ProtoWriter,
    pub state: Arc<ServerState>,
    pub peer_id: String,
    pub request_id: u64,
    pub project_id: Option<ProjectId>,
    pub object: UploadObject,
    pub transfer: Transfer,
    pub metadata: UploadMetadata,
    pub permit: UploadPermit,
}

pub(super) async fn run(c: Commit) {
    let outcome = match (&c.object, c.metadata) {
        (UploadObject::Nar { store_path }, UploadMetadata::Nar(meta)) => {
            commit_nar(&c.state, c.project_id, store_path, c.transfer, &meta).await
        }
        (UploadObject::EvalCache { fingerprint }, UploadMetadata::EvalCache { size_bytes }) => {
            commit_eval_cache(&c.state, fingerprint, c.transfer, size_bytes).await
        }
        _ => UploadOutcome::Rejected {
            reason: "metadata does not match the upload object".into(),
        },
    };
    if !matches!(outcome, UploadOutcome::Ok) {
        warn!(peer_id = %c.peer_id, request_id = c.request_id, ?outcome, "upload commit did not land");
    }
    match outcome {
        UploadOutcome::Ok => c.permit.committed(),
        _ => drop(c.permit),
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

async fn commit_nar(
    state: &Arc<ServerState>,
    project_id: Option<ProjectId>,
    store_path: &str,
    transfer: Transfer,
    meta: &NarUploadMetadata,
) -> UploadOutcome {
    let Some(ObjectKey::Nar(hash)) = super::object_key(&UploadObject::Nar {
        store_path: store_path.to_owned(),
    }) else {
        return rejected(format!("malformed store path {store_path}"));
    };
    let placed = match transfer {
        Transfer::Relay(writer) => place_relayed(state, &hash, *writer, meta).await,
        Transfer::Put | Transfer::Multipart { .. } => place_presigned(state, &hash, meta).await,
    };
    if let Err(outcome) = placed {
        return outcome;
    }
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
    UploadOutcome::Ok
}

async fn place_relayed(
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

async fn commit_eval_cache(
    state: &Arc<ServerState>,
    fingerprint: &str,
    transfer: Transfer,
    size_bytes: u64,
) -> UploadOutcome {
    match transfer {
        Transfer::Relay(writer) => {
            let staged = match (*writer).finish().await {
                Ok(s) => s,
                Err(e) => return retry(e),
            };
            if staged.len != size_bytes {
                return rejected(format!(
                    "received {} bytes, reported {size_bytes}",
                    staged.len
                ));
            }
            let bytes = match tokio::fs::read(&staged.path).await {
                Ok(b) => b,
                Err(e) => return retry(e),
            };
            let _ = tokio::fs::remove_file(&staged.path).await;
            if let Err(e) = state.nar_storage.put_eval_cache(fingerprint, bytes).await {
                return retry(e);
            }
        }
        Transfer::Put | Transfer::Multipart { .. } => {
            if let Err(e) = state
                .nar_storage
                .verify_eval_cache(fingerprint, size_bytes)
                .await
            {
                return retry(e);
            }
        }
    }
    super::super::eval_cache::record_eval_cache(state, fingerprint, size_bytes).await;
    UploadOutcome::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_storage::admission::{Admitted, ObjectKey};
    use gradient_test_support::state::test_state;
    use gradient_wire::session::frame::WireMessage as _;
    use sea_orm::{DatabaseBackend, MockDatabase};

    async fn granted_permit(
        state: &Arc<ServerState>,
    ) -> (
        gradient_storage::admission::AdmissionSession,
        gradient_storage::admission::UploadPermit,
    ) {
        let (session, mut rx) = state.upload_admission.open_session("test");
        session.request(1, ObjectKey::Nar("c".repeat(32)), 3);
        let Some(Admitted::Granted { permit, .. }) = rx.recv().await else {
            panic!("granted")
        };
        (session, permit)
    }

    #[tokio::test]
    async fn a_relayed_nar_with_the_wrong_size_is_rejected_and_frees_its_permit() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let (_session, permit) = granted_permit(&state).await;
        let dir = tempfile::TempDir::new().unwrap();
        let partials =
            gradient_storage::PartialStore::new(dir.path(), std::time::Duration::from_secs(60))
                .unwrap();
        let mut writer = partials.open_writer("peer/c", "c", 0, 0).await.unwrap();
        writer.append(0, b"abc").await.unwrap();
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            state: Arc::clone(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project_id: None,
            object: UploadObject::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
            },
            transfer: Transfer::Relay(Box::new(writer)),
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

        let msg = ServerMessage::decode(sent.try_recv().unwrap())
            .unwrap()
            .into_message()
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
            project_id: None,
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

        let msg = ServerMessage::decode(sent.try_recv().unwrap())
            .unwrap()
            .into_message()
            .unwrap();
        assert!(matches!(
            msg,
            ServerMessage::UploadCommitted {
                request_id: 1,
                outcome: UploadOutcome::Retry { .. }
            }
        ));
    }

    /// A resumed prefix may come from a differently configured encoder; a
    /// mismatch then starts the upload over instead of failing the job.
    #[tokio::test]
    async fn a_resumed_relay_that_fails_its_hash_is_retried_from_scratch() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let (_session, permit) = granted_permit(&state).await;
        let dir = tempfile::TempDir::new().unwrap();
        tokio::fs::create_dir_all(dir.path().join("peer"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("peer/c.partial"), b"ab")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("peer/c.token"), b"c")
            .await
            .unwrap();
        let partials =
            gradient_storage::PartialStore::new(dir.path(), std::time::Duration::from_secs(60))
                .unwrap();
        let mut writer = partials.open_writer("peer/c", "c", 2, 0).await.unwrap();
        assert!(writer.resumed());
        writer.append(2, b"c").await.unwrap();
        let (writer_out, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        run(Commit {
            writer: writer_out,
            state: Arc::clone(&state),
            peer_id: "w1".into(),
            request_id: 1,
            project_id: None,
            object: UploadObject::Nar {
                store_path: format!("/nix/store/{}-p", "c".repeat(32)),
            },
            transfer: Transfer::Relay(Box::new(writer)),
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

        let msg = ServerMessage::decode(sent.try_recv().unwrap())
            .unwrap()
            .into_message()
            .unwrap();
        assert!(matches!(
            msg,
            ServerMessage::UploadCommitted {
                request_id: 1,
                outcome: UploadOutcome::Retry { .. }
            }
        ));
        assert!(
            !dir.path().join("peer/c.partial").exists(),
            "the foreign prefix is gone"
        );
    }
}
