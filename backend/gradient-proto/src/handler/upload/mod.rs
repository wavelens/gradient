/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod commit;
mod table;

use std::sync::Arc;
use std::time::{Duration, Instant};

use gradient_core::ServerState;
use gradient_storage::admission::{AdmissionSession, Admitted, ObjectKey};
use gradient_storage::{PartialStore, StorageTarget, upload_lease};
use gradient_wire::messages::ServerMessage;
use gradient_wire::types::{GrantTarget, UploadMetadata, UploadObject, UploadOutcome};
use tracing::warn;

use super::inbound::InboundContext;
use super::socket::send_server_msg;
pub(super) use table::{Granted, Lease, Transfer, UploadTable};

pub(super) struct UploadSession {
    pub admission: AdmissionSession,
    pub table: UploadTable,
    pub partials: PartialStore,
    pub retain_up_to: u64,
    pub idle_lease: Duration,
}

pub(super) fn object_key(object: &UploadObject) -> Option<ObjectKey> {
    match object {
        UploadObject::Nar { store_path } => {
            let name = store_path.strip_prefix("/nix/store/")?;
            let hash = name.split('-').next()?;
            (hash.len() == 32).then(|| ObjectKey::Nar(hash.to_owned()))
        }
        UploadObject::EvalCache { fingerprint } => Some(ObjectKey::EvalCache(fingerprint.clone())),
    }
}

fn key_name(key: &ObjectKey) -> &str {
    match key {
        ObjectKey::Nar(s) | ObjectKey::EvalCache(s) | ObjectKey::Rest(s) => s,
    }
}

impl InboundContext<'_> {
    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn on_upload_request(
        &mut self,
        job_id: String,
        request_id: u64,
        object: UploadObject,
        size: u64,
        uploads: &mut UploadSession,
    ) {
        let Some(key) = object_key(&object) else {
            return self
                .settle(
                    request_id,
                    UploadOutcome::Rejected {
                        reason: format!("malformed upload object {object:?}"),
                    },
                )
                .await;
        };
        if !self.active.contains(&job_id) {
            return self
                .settle(
                    request_id,
                    UploadOutcome::Rejected {
                        reason: format!("job {job_id} is not running on this session"),
                    },
                )
                .await;
        }
        if let UploadObject::EvalCache { fingerprint } = &object
            && !super::eval_cache::accepts_push(self.state, fingerprint, size).await
        {
            return self.grant(request_id, GrantTarget::Skip).await;
        }
        let priority = gradient_wire::messages::is_small_upload(size);
        if !uploads.table.queue(request_id, job_id, object, size) {
            return self
                .settle(
                    request_id,
                    UploadOutcome::Rejected {
                        reason: format!("request {request_id} is already open"),
                    },
                )
                .await;
        }
        uploads.admission.request(request_id, key, size, priority);
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn on_upload_admitted(
        &mut self,
        admitted: Admitted,
        uploads: &mut UploadSession,
    ) {
        match admitted {
            Admitted::Skip { id } => {
                uploads.table.take_queued(id);
                self.grant(id, GrantTarget::Skip).await;
            }
            Admitted::Granted {
                id,
                object: key,
                permit,
            } => {
                let Some(queued) = uploads.table.take_queued(id) else {
                    return;
                };
                match self.already_stored(&key).await {
                    Ok(false) => {}
                    Ok(true) => {
                        permit.committed();
                        return self.grant(id, GrantTarget::Skip).await;
                    }
                    Err(e) => {
                        drop(permit);
                        return self
                            .settle(
                                id,
                                UploadOutcome::Retry {
                                    reason: format!("checking the cache index: {e:#}"),
                                },
                            )
                            .await;
                    }
                }
                match self.open_transfer(&key, queued.size, uploads).await {
                    Ok((target, transfer, lease)) => {
                        uploads.table.grant(
                            id,
                            Granted {
                                job_id: queued.job_id,
                                object: queued.object,
                                size: queued.size,
                                permit,
                                transfer,
                                lease,
                                finished: None,
                                final_seen: false,
                            },
                        );
                        self.grant(id, target).await;
                    }
                    Err(e) => {
                        warn!(peer_id = %self.peer_id, request_id = id, error = %e, "upload target failed");
                        drop(permit);
                        self.settle(
                            id,
                            UploadOutcome::Retry {
                                reason: format!("{e:#}"),
                            },
                        )
                        .await;
                    }
                }
            }
        }
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn already_stored(&self, key: &ObjectKey) -> Result<bool, sea_orm::DbErr> {
        let ObjectKey::Nar(hash) = key else {
            return Ok(false);
        };

        Ok(crate::import::stored_path(&self.state.worker_db, hash)
            .await?
            .is_some())
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn open_transfer(
        &self,
        key: &ObjectKey,
        size: u64,
        uploads: &UploadSession,
    ) -> anyhow::Result<(GrantTarget, Transfer, Lease)> {
        let target = self.state.nar_storage.upload_target(key, size).await?;
        let now = Instant::now();
        let lease = match upload_lease(&target, size) {
            Some(ttl) => Lease::Until(now + ttl),
            None => Lease::Idle {
                last: now,
                idle: uploads.idle_lease,
            },
        };
        Ok(match target {
            StorageTarget::Passthrough => {
                let token = key_name(key);
                let partial = format!("{}/{token}", self.peer_id);
                let received = uploads.partials.received_len(&partial, token).await?;
                let writer = uploads
                    .partials
                    .open_writer(&partial, token, received, uploads.retain_up_to)
                    .await?;
                (
                    GrantTarget::Passthrough {
                        resume_offset: received,
                    },
                    Transfer::Passthrough(Box::new(writer)),
                    lease,
                )
            }
            StorageTarget::Put { url } => (GrantTarget::Put { url }, Transfer::Put, lease),
            StorageTarget::Multipart(grant) => {
                let upload_id = grant.upload_id.clone();
                (
                    GrantTarget::Multipart(grant),
                    Transfer::Multipart { upload_id },
                    lease,
                )
            }
        })
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn on_upload_chunk(
        &mut self,
        request_id: u64,
        offset: u64,
        data: &[u8],
        is_final: bool,
        uploads: &mut UploadSession,
    ) {
        let verdict = match uploads.table.granted_mut(request_id) {
            Some(Granted {
                transfer: Transfer::Passthrough(writer),
                size,
                lease,
                final_seen,
                ..
            }) => {
                let bound = *size + *size / 128 + 1024 * 1024;
                if offset != writer.len() {
                    Err(format!(
                        "chunk at offset {offset} follows {} bytes",
                        writer.len()
                    ))
                } else if offset + data.len() as u64 > bound {
                    Err(format!("upload exceeds {bound} bytes"))
                } else {
                    lease.touch(Instant::now());
                    *final_seen |= is_final;
                    writer
                        .append(offset, data)
                        .await
                        .map_err(|e| format!("staging failed: {e:#}"))
                }
            }
            _ => Err(format!("request {request_id} holds no passthrough grant")),
        };
        if let Err(reason) = verdict {
            if let Some(granted) = uploads.table.remove(request_id) {
                self.abandon(granted).await;
            }
            return self
                .settle(request_id, UploadOutcome::Rejected { reason })
                .await;
        }
        if is_final && let Some(granted) = uploads.table.take_granted(request_id) {
            match granted.finished {
                Some(_) => self.commit(request_id, granted),
                None => uploads.table.grant(request_id, granted),
            }
        }
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub(super) async fn on_upload_finished(
        &mut self,
        request_id: u64,
        metadata: UploadMetadata,
        uploads: &mut UploadSession,
    ) {
        let Some(mut granted) = uploads.table.take_granted(request_id) else {
            return self
                .settle(
                    request_id,
                    UploadOutcome::Rejected {
                        reason: format!("request {request_id} holds no grant"),
                    },
                )
                .await;
        };
        granted.finished = Some(metadata);
        if matches!(granted.transfer, Transfer::Passthrough(_)) && !granted.final_seen {
            return uploads.table.grant(request_id, granted);
        }
        self.commit(request_id, granted);
    }

    fn commit(&self, request_id: u64, granted: Granted) {
        let Some(metadata) = granted.finished else {
            return;
        };
        let project = {
            let scheduler = Arc::clone(self.scheduler);
            let state = Arc::clone(self.state);
            let peer_id = self.peer_id.to_owned();
            let job_id = granted.job_id.clone();
            Box::pin(async move {
                match scheduler.project_for_job(&job_id).await {
                    Some(id) => Some(id),
                    None => {
                        super::nar::project_for_dispatched_job(&state.worker_db, &peer_id, &job_id)
                            .await
                    }
                }
            })
        };
        let c = commit::Commit {
            writer: self.writer.clone(),
            state: Arc::clone(self.state),
            peer_id: self.peer_id.to_owned(),
            request_id,
            project,
            object: granted.object,
            transfer: granted.transfer,
            metadata,
            permit: granted.permit,
        };
        self.state.shutdown.spawn(commit::run(c));
    }

    pub(super) async fn on_upload_cancel(&mut self, request_id: u64, uploads: &mut UploadSession) {
        uploads.admission.cancel(request_id);
        if let Some(granted) = uploads.table.remove(request_id) {
            self.abandon(granted).await;
        }
    }

    pub(super) async fn forget_uploads(&mut self, job_id: &str, uploads: &mut UploadSession) {
        for (id, granted) in uploads.table.forget_job(job_id) {
            uploads.admission.cancel(id);
            if let Some(granted) = granted {
                self.abandon(granted).await;
            }
        }
    }

    pub(super) async fn sweep_uploads(&mut self, uploads: &mut UploadSession) {
        for (id, granted) in uploads.table.expired(Instant::now()) {
            self.abandon(granted).await;
            self.settle(
                id,
                UploadOutcome::Retry {
                    reason: "upload lease expired".into(),
                },
            )
            .await;
        }
    }

    async fn abandon(&self, granted: Granted) {
        abandon_transfer(self.state, granted).await;
    }
}

pub(super) async fn abandon_transfer(state: &ServerState, granted: Granted) {
    if let (Transfer::Multipart { upload_id }, Some(ObjectKey::Nar(hash))) =
        (&granted.transfer, object_key(&granted.object))
    {
        state.nar_storage.abort_multipart(&hash, upload_id).await;
    }
}

impl InboundContext<'_> {
    async fn grant(&self, request_id: u64, target: GrantTarget) {
        let _ = send_server_msg(
            self.writer,
            &ServerMessage::UploadGrant { request_id, target },
        )
        .await;
    }

    pub(super) async fn settle(&self, request_id: u64, outcome: UploadOutcome) {
        let _ = send_server_msg(
            self.writer,
            &ServerMessage::UploadCommitted {
                request_id,
                outcome,
            },
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::inbound::fixture::{JOB, TestSession, decode};
    use gradient_test_support::state::test_state;
    use gradient_types::MCachedPath;
    use gradient_wire::messages::ClientMessage;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn nar(c: char) -> UploadObject {
        UploadObject::Nar {
            store_path: format!("/nix/store/{}-p", c.to_string().repeat(32)),
        }
    }

    fn unknown_path() -> sea_orm::DatabaseConnection {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .into_connection()
    }

    #[tokio::test]
    async fn a_request_for_a_stored_path_is_skipped_without_a_transfer() {
        let stored = MCachedPath {
            hash: "h".repeat(32),
            file_hash: Some("sha256:stored".into()),
            confirmed: true,
            ..Default::default()
        };
        let state = test_state(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![stored]])
                .into_connection(),
        );
        let (mut session, mut sent, mut admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();
        ctx.on_upload_request(JOB.into(), 1, nar('h'), 1024, uploads)
            .await;
        ctx.on_upload_admitted(admitted.recv().await.unwrap(), uploads)
            .await;
        assert!(matches!(
            decode(sent.try_recv().unwrap()),
            ServerMessage::UploadGrant {
                request_id: 1,
                target: GrantTarget::Skip
            }
        ));
        assert_eq!(state.upload_admission.in_flight(), 0);
    }

    #[tokio::test]
    async fn a_request_for_a_job_this_session_does_not_run_is_rejected() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let (mut session, mut sent, _admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();
        ctx.on_upload_request("build:unknown".into(), 1, nar('a'), 1, uploads)
            .await;
        assert!(matches!(
            decode(sent.try_recv().unwrap()),
            ServerMessage::UploadCommitted {
                request_id: 1,
                outcome: UploadOutcome::Rejected { .. }
            }
        ));
        assert_eq!(state.upload_admission.in_flight(), 0);
    }

    #[tokio::test]
    async fn a_request_on_the_local_backend_is_granted_a_passthrough() {
        let state = test_state(unknown_path());
        let (mut session, mut sent, mut admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();
        ctx.on_upload_request(JOB.into(), 1, nar('b'), 1024, uploads)
            .await;
        ctx.on_upload_admitted(admitted.recv().await.unwrap(), uploads)
            .await;
        assert!(matches!(
            decode(sent.try_recv().unwrap()),
            ServerMessage::UploadGrant {
                request_id: 1,
                target: GrantTarget::Passthrough { resume_offset: 0 }
            }
        ));
        assert_eq!(state.upload_admission.in_flight(), 1);
    }

    #[tokio::test]
    async fn a_chunk_before_its_grant_is_rejected_and_not_staged() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let (mut session, mut sent, _admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();
        ctx.on_upload_chunk(9, 0, b"bytes", false, uploads).await;
        assert!(matches!(
            decode(sent.try_recv().unwrap()),
            ServerMessage::UploadCommitted {
                request_id: 9,
                outcome: UploadOutcome::Rejected { .. }
            }
        ));
    }

    #[tokio::test]
    async fn a_granted_passthrough_accepts_contiguous_chunks_and_rejects_a_gap() {
        let state = test_state(unknown_path());
        let (mut session, mut sent, mut admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();
        ctx.on_upload_request(JOB.into(), 1, nar('c'), 1024, uploads)
            .await;
        ctx.on_upload_admitted(admitted.recv().await.unwrap(), uploads)
            .await;
        let _grant = sent.try_recv().unwrap();

        ctx.on_upload_chunk(1, 0, &[0u8; 16], false, uploads).await;
        assert!(
            sent.try_recv().is_err(),
            "a contiguous chunk is accepted silently"
        );
        ctx.on_upload_chunk(1, 32, &[0u8; 16], false, uploads).await;
        assert!(matches!(
            decode(sent.try_recv().unwrap()),
            ServerMessage::UploadCommitted {
                request_id: 1,
                outcome: UploadOutcome::Rejected { .. }
            }
        ));
        assert_eq!(state.upload_admission.in_flight(), 0);
    }

    #[tokio::test]
    async fn an_expired_grant_is_told_to_retry_and_frees_its_permit() {
        let state = test_state(unknown_path());
        let (mut session, mut sent, mut admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();
        uploads.idle_lease = Duration::ZERO;
        ctx.on_upload_request(JOB.into(), 1, nar('d'), 8, uploads)
            .await;
        ctx.on_upload_admitted(admitted.recv().await.unwrap(), uploads)
            .await;
        let _grant = sent.try_recv().unwrap();

        tokio::time::sleep(Duration::from_millis(5)).await;
        ctx.sweep_uploads(uploads).await;

        assert!(matches!(
            decode(sent.try_recv().unwrap()),
            ServerMessage::UploadCommitted {
                request_id: 1,
                outcome: UploadOutcome::Retry { .. }
            }
        ));
        assert_eq!(state.upload_admission.in_flight(), 0);
    }

    /// The worker's writer is draining the control lane first, and `UploadFinished` can overtake
    /// the last chunks. The commit must wait for them.
    #[tokio::test]
    async fn a_finish_that_overtakes_the_final_chunk_waits_for_it() {
        let state = test_state(unknown_path());
        let (mut session, mut sent, mut admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();
        let body = b"compressed nar bytes".to_vec();
        ctx.on_upload_request(JOB.into(), 1, nar('e'), 64, uploads)
            .await;
        ctx.on_upload_admitted(admitted.recv().await.unwrap(), uploads)
            .await;
        let _grant = sent.try_recv().unwrap();

        ctx.on_upload_chunk(1, 0, &body[..8], false, uploads).await;
        ctx.on_upload_finished(
            1,
            UploadMetadata::Nar(Box::new(gradient_wire::types::NarUploadMetadata {
                file_hash: gradient_storage::file_hash_sri(&body),
                file_size: body.len() as u64,
                nar_size: 64,
                nar_hash: "sha256:n".into(),
                references: Vec::new(),
                deriver: None,
                ca: None,
                multipart: None,
            })),
            uploads,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            sent.try_recv().is_err(),
            "nothing is committed before the final chunk"
        );

        ctx.on_upload_chunk(1, 8, &body[8..], false, uploads).await;
        ctx.on_upload_chunk(1, body.len() as u64, &[], true, uploads)
            .await;
        let committed = tokio::time::timeout(Duration::from_secs(5), sent.recv())
            .await
            .expect("the commit answers")
            .unwrap();
        assert!(
            !matches!(
                decode(committed),
                ServerMessage::UploadCommitted {
                    outcome: UploadOutcome::Rejected { .. },
                    ..
                }
            ),
            "the complete passthrough is not rejected"
        );
    }

    #[tokio::test]
    async fn a_failed_job_releases_its_granted_and_queued_uploads() {
        let state = test_state(unknown_path());
        let (mut session, _sent, mut admitted) = TestSession::new(&state).await;
        let (mut ctx, uploads) = session.split();
        ctx.on_upload_request(JOB.into(), 1, nar('f'), 8, uploads)
            .await;
        ctx.on_upload_admitted(admitted.recv().await.unwrap(), uploads)
            .await;
        ctx.on_upload_request(JOB.into(), 2, nar('g'), 8, uploads)
            .await;
        assert_eq!(state.upload_admission.in_flight(), 2);

        let failed = ClientMessage::JobFailed {
            job_id: JOB.into(),
            assignment_id: "not-this-dispatch".into(),
            error: "boom".into(),
            kind: gradient_wire::messages::BuildFailureKind::Transient,
            missing_paths: Vec::new(),
            spans: Vec::new(),
            elapsed_ms: 0,
            metrics: None,
        };
        ctx.handle(failed, uploads).await;

        assert_eq!(state.upload_admission.in_flight(), 0);
        assert!(uploads.table.is_empty());
    }
}
