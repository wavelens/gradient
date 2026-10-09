/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use gradient_core::ServerState;
use gradient_graph::Demotion;
use gradient_storage::passthrough::{
    PassthroughError, PassthroughLimits, PassthroughRequest, send_nar,
};
use gradient_util::telemetry::{GAUGES, Gauges};
use gradient_wire::messages::ServerMessage;
use gradient_wire::session::frame::send_server_msg;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::warn;

use super::socket::ProtoWriter;

pub(super) struct ServeSlot {
    _connection: OwnedSemaphorePermit,
    _server: OwnedSemaphorePermit,
    gauges: &'static Gauges,
}

struct Waiting(&'static Gauges);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.serves_waiting.dec();
    }
}

impl ServeSlot {
    pub(super) async fn acquire(
        connection: Arc<Semaphore>,
        server: Arc<Semaphore>,
    ) -> Option<Self> {
        Self::acquire_with(connection, server, &GAUGES).await
    }

    /// The connection's own permit comes first. A connection queueing many paths must not hold
    /// server-wide permits while it waits for its own.
    pub(super) async fn acquire_with(
        connection: Arc<Semaphore>,
        server: Arc<Semaphore>,
        gauges: &'static Gauges,
    ) -> Option<Self> {
        gauges.serves_waiting.inc();
        let waiting = Waiting(gauges);
        let connection = connection.acquire_owned().await.ok()?;
        let server = server.acquire_owned().await.ok()?;
        drop(waiting);
        gauges.serves_active.inc();

        Some(Self {
            _connection: connection,
            _server: server,
            gauges,
        })
    }
}

impl Drop for ServeSlot {
    fn drop(&mut self) {
        self.gauges.serves_active.dec();
    }
}

pub(super) async fn serve_nar_request(
    state: &Arc<ServerState>,
    writer: &ProtoWriter,
    job_id: &str,
    store_path: &str,
    resume_from: u64,
    client_token: Option<&str>,
) -> anyhow::Result<()> {
    let nar_cfg = &state.config.nar;
    let limits = PassthroughLimits {
        open: Duration::from_secs(nar_cfg.storage_open_timeout_secs),
        chunk_read: Duration::from_secs(nar_cfg.send_chunk_timeout_secs),
        chunk_bytes: nar_cfg.chunk_bytes as usize,
    };
    let Some(key) = store_hash(store_path) else {
        let reason = format!("invalid store path: {store_path}");
        unavailable(writer, job_id, store_path, &reason).await;
        return Err(anyhow::anyhow!(reason));
    };
    let req = PassthroughRequest {
        job_id,
        store_path,
        key,
        resume_from,
        client_token,
    };
    match send_nar(&state.nar_storage, writer, req, limits).await {
        Ok(_) => Ok(()),
        Err(PassthroughError::NotFound(reason)) => {
            unavailable(writer, job_id, store_path, &reason).await;
            invalidate_cached_path(state, key, store_path).await;
            Err(anyhow::anyhow!(reason))
        }
        Err(e) => Err(e.into()),
    }
}

async fn unavailable(writer: &ProtoWriter, job_id: &str, store_path: &str, reason: &str) {
    let msg = ServerMessage::NarUnavailable {
        job_id: job_id.to_owned(),
        store_path: store_path.to_owned(),
        reason: reason.to_owned(),
    };
    let _ = send_server_msg(writer, &msg).await;
}

async fn invalidate_cached_path(state: &Arc<ServerState>, hash: &str, store_path: &str) {
    if awaiting_upload(state, hash).await {
        warn!(%hash, %store_path, "NAR not on this instance yet; the row is a fresh staged upload");
        return;
    }

    state.nar_storage.hot().invalidate(hash);
    match state
        .graph
        .demote(Demotion::MissingNar {
            hash: hash.to_owned(),
        })
        .await
    {
        Ok(_) => warn!(
            %hash,
            %store_path,
            "self-heal: NAR missing from storage; cached_path demoted so the path will be rebuilt"
        ),
        Err(e) => {
            warn!(%hash, %store_path, error = %e, "self-heal: failed to demote cached output")
        }
    }
}

/// An unconfirmed row younger than the upload grace is staged by another instance and not missing.
async fn awaiting_upload(state: &Arc<ServerState>, hash: &str) -> bool {
    use gradient_entity::cached_path::{Column as CCachedPath, Entity as ECachedPath};
    use sea_orm::{ColumnTrait as _, EntityTrait as _, QueryFilter as _};

    let grace = chrono::Duration::hours(state.config.gc.nar_upload_grace_hours.max(0));
    match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(&state.worker_db)
        .await
    {
        Ok(Some(row)) => !row.confirmed && row.created_at > gradient_types::now() - grace,
        _ => false,
    }
}

fn store_hash(store_path: &str) -> Option<&str> {
    let hash = store_path
        .strip_prefix("/nix/store/")
        .unwrap_or(store_path)
        .split('-')
        .next()?;
    (hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(hash)
}

#[cfg(test)]
mod serve_nar_tests {
    use super::*;
    use bytes::Bytes;
    use gradient_test_support::state::test_state;
    use gradient_wire::messages::ServerMessage;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use tokio::sync::mpsc;

    fn spy_writer(timeout: Duration) -> (ProtoWriter, mpsc::Receiver<Bytes>) {
        ProtoWriter::spy(timeout)
    }

    fn decode(bytes: Bytes) -> ServerMessage {
        gradient_wire::codec::from_bytes::<ServerMessage>(
            bytes,
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .expect("deserialise ServerMessage")
    }

    #[tokio::test]
    async fn serve_streams_full_payload_in_chunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        let mut payload = Vec::with_capacity(9 * 1024 * 1024);
        for i in 0..(9 * 1024 * 1024 / 4) {
            payload.extend_from_slice(&(i as u32).to_le_bytes());
        }
        let hash = "abcdefghijklmnopqrstuvwxyz012345";
        state.nar_storage.put(hash, payload.clone()).await.unwrap();

        let (writer, mut rx) = spy_writer(Duration::from_secs(5));
        let store_path = format!("/nix/store/{hash}-test-pkg");
        serve_nar_request(&state, &writer, "job-1", &store_path, 0, None)
            .await
            .expect("serve must succeed");

        let mut assembled = Vec::with_capacity(payload.len());
        let mut nar_push_frames = 0u32;
        let mut saw_header = false;
        let mut saw_final = false;
        while let Ok(bytes) = rx.try_recv() {
            match decode(bytes) {
                ServerMessage::NarStreamHeader { total_bytes, .. } => {
                    saw_header = true;
                    assert!(!saw_final, "header must precede chunks");
                    assert_eq!(total_bytes as usize, payload.len());
                }
                ServerMessage::NarPush { data, is_final, .. } => {
                    assembled.extend_from_slice(&data);
                    if is_final {
                        saw_final = true;
                    }
                    nar_push_frames += 1;
                }
                other => panic!("unexpected frame: {}", other.variant_name()),
            }
        }
        assert!(saw_header, "a NarStreamHeader must precede the chunks");
        assert!(
            nar_push_frames >= 3,
            "9 MiB in 512 KiB chunks is at least 3 frames, got {nar_push_frames}"
        );
        assert!(saw_final, "the last frame must be is_final=true");
        assert_eq!(
            assembled, payload,
            "concatenated NarPush data must equal source"
        );
    }

    #[tokio::test]
    async fn serve_emits_nar_unavailable_when_missing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        let (writer, mut rx) = spy_writer(Duration::from_secs(5));

        let res = serve_nar_request(
            &state,
            &writer,
            "job-1",
            "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-missing",
            0,
            None,
        )
        .await;
        assert!(res.is_err(), "missing path must surface as Err");

        let bytes = rx.try_recv().expect("expect one frame");
        let msg = decode(bytes);
        assert_eq!(msg.variant_name(), "NarUnavailable");
        assert!(
            rx.try_recv().is_err(),
            "no further frames after NarUnavailable"
        );
    }

    #[tokio::test]
    async fn serve_answers_a_hot_entry_in_bulk_chunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let cli = gradient_test_support::prelude::test_cli();
        let storage = gradient_storage::NarStore::local(&cli.server.base_dir)
            .unwrap()
            .with_hot_cache(gradient_storage::HotNarCache::new(
                4 * 1024 * 1024,
                2 * 1024 * 1024,
            ));
        let state = gradient_test_support::prelude::test_state_with_storage(db, storage);
        let payload: Vec<u8> = (0..(1024 * 1024 + 123)).map(|i| i as u8).collect();
        let hash = "abcdefghijklmnopqrstuvwxyz012345";
        state
            .nar_storage
            .hot()
            .insert(hash, Bytes::from(payload.clone()));

        let (writer, mut rx) = spy_writer(Duration::from_secs(5));
        let store_path = format!("/nix/store/{hash}-test-pkg");
        serve_nar_request(&state, &writer, "job-1", &store_path, 0, None)
            .await
            .expect("serve from RAM");

        let mut assembled = Vec::new();
        let mut frames = 0u32;
        while let Ok(bytes) = rx.try_recv() {
            match decode(bytes) {
                ServerMessage::NarStreamHeader { total_bytes, .. } => {
                    assert_eq!(total_bytes as usize, payload.len());
                }
                ServerMessage::NarPush { data, .. } => {
                    assembled.extend_from_slice(&data);
                    frames += 1;
                }
                other => panic!("unexpected frame: {}", other.variant_name()),
            }
        }
        assert_eq!(assembled, payload);
        assert!(
            frames >= 3,
            "1 MiB in 512 KiB chunks is at least three frames"
        );
        assert_eq!(state.nar_storage.hot().stats().hits, 1);
    }

    #[tokio::test]
    async fn an_invalid_store_path_is_unavailable() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_state(db);
        let (writer, mut rx) = spy_writer(Duration::from_secs(5));

        let res = serve_nar_request(&state, &writer, "job-1", "not-a-store-path", 0, None).await;

        assert!(res.is_err());
        let msg = decode(rx.try_recv().expect("one frame"));
        assert_eq!(msg.variant_name(), "NarUnavailable");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_young_unconfirmed_row_is_awaiting_upload_and_an_old_or_confirmed_one_is_not() {
        fn row(confirmed: bool, age: chrono::Duration) -> gradient_entity::cached_path::Model {
            gradient_entity::cached_path::Model {
                hash: "abcdefghijklmnopqrstuvwxyz012345".into(),
                file_hash: Some("sha256:x".into()),
                confirmed,
                created_at: gradient_types::now() - age,
                ..Default::default()
            }
        }

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![row(false, chrono::Duration::minutes(1))],
                vec![row(true, chrono::Duration::minutes(1))],
                vec![row(false, chrono::Duration::hours(48))],
            ])
            .into_connection();
        let state = test_state(db);
        let hash = "abcdefghijklmnopqrstuvwxyz012345";

        assert!(awaiting_upload(&state, hash).await, "young and unconfirmed");
        assert!(!awaiting_upload(&state, hash).await, "confirmed");
        assert!(!awaiting_upload(&state, hash).await, "older than the grace");
    }
}

#[cfg(test)]
mod serve_slot_tests {
    use super::*;
    use gradient_util::telemetry::Gauges;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Semaphore;

    fn gauges() -> &'static Gauges {
        Box::leak(Box::new(Gauges::new()))
    }

    fn permits(n: usize) -> Arc<Semaphore> {
        Arc::new(Semaphore::new(n))
    }

    async fn take(
        connection: &Arc<Semaphore>,
        server: &Arc<Semaphore>,
        g: &'static Gauges,
    ) -> Option<ServeSlot> {
        ServeSlot::acquire_with(Arc::clone(connection), Arc::clone(server), g).await
    }

    #[tokio::test]
    async fn a_slot_is_active_until_dropped() {
        let g = gauges();
        let slot = take(&permits(1), &permits(1), g).await.expect("slot");

        assert_eq!(g.serves_active.get(), 1);
        assert_eq!(g.serves_waiting.get(), 0);
        drop(slot);
        assert_eq!(g.serves_active.get(), 0);
    }

    #[tokio::test]
    async fn a_queued_serve_counts_as_waiting() {
        let g = gauges();
        let (sem, server) = (permits(1), permits(8));
        let held = take(&sem, &server, g).await.expect("slot");
        let mut queued = Box::pin(take(&sem, &server, g));
        let still_queued = tokio::time::timeout(Duration::from_millis(20), &mut queued).await;

        assert!(still_queued.is_err());
        assert_eq!(g.serves_waiting.get(), 1);
        drop(held);
        let slot = queued.await.expect("slot");
        assert_eq!(g.serves_waiting.get(), 0);
        assert_eq!(g.serves_active.get(), 1);
        drop(slot);
    }

    #[tokio::test]
    async fn the_server_wide_limit_holds_back_another_connection() {
        let g = gauges();
        let server = permits(1);
        let held = take(&permits(8), &server, g).await.expect("slot");
        let other_connection = permits(8);
        let mut other = Box::pin(take(&other_connection, &server, g));

        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut other)
                .await
                .is_err()
        );
        drop(held);
        assert!(other.await.is_some());
    }

    #[tokio::test]
    async fn cancelled_wait_releases_waiting() {
        let g = gauges();
        let (sem, server) = (permits(0), permits(1));
        let waiting = take(&sem, &server, g);
        let cancelled = tokio::time::timeout(Duration::from_millis(20), waiting).await;

        assert!(cancelled.is_err());
        assert_eq!(g.serves_waiting.get(), 0);
        assert_eq!(g.serves_active.get(), 0);
    }

    #[tokio::test]
    async fn a_closed_semaphore_yields_no_slot() {
        let g = gauges();
        let sem = permits(0);
        sem.close();

        assert!(take(&sem, &permits(1), g).await.is_none());
        assert_eq!(g.serves_waiting.get(), 0);
    }
}
