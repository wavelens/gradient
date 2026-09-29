/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Serving NARs from `nar_storage` to a worker, and self-healing a row whose
//! object has vanished.

use std::sync::Arc;
use std::time::Duration;

use gradient_core::ServerState;
use gradient_graph::Demotion;
use gradient_storage::relay::{RelayRequest, RelayTimeouts, ServeError, serve_nar};
use gradient_util::telemetry::{GAUGES, Gauges};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::warn;

use super::socket::ProtoWriter;

pub(super) struct ServeSlot {
    _permit: OwnedSemaphorePermit,
    gauges: &'static Gauges,
}

struct Waiting(&'static Gauges);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.serves_waiting.dec();
    }
}

impl ServeSlot {
    pub(super) async fn acquire(semaphore: Arc<Semaphore>) -> Option<Self> {
        Self::acquire_with(semaphore, &GAUGES).await
    }

    pub(super) async fn acquire_with(
        semaphore: Arc<Semaphore>,
        gauges: &'static Gauges,
    ) -> Option<Self> {
        gauges.serves_waiting.inc();
        let waiting = Waiting(gauges);
        let permit = semaphore.acquire_owned().await.ok()?;
        drop(waiting);
        gauges.serves_active.inc();

        Some(Self {
            _permit: permit,
            gauges,
        })
    }
}

impl Drop for ServeSlot {
    fn drop(&mut self) {
        self.gauges.serves_active.dec();
    }
}

/// Stream a single requested NAR from `nar_storage` to the worker, purging
/// the `cached_path` row when the object has vanished from storage.
pub(super) async fn serve_nar_request(
    state: &Arc<ServerState>,
    writer: &ProtoWriter,
    job_id: &str,
    store_path: &str,
    resume_from: u64,
    client_token: Option<&str>,
) -> anyhow::Result<()> {
    let nar_cfg = &state.config.nar;
    let timeouts = RelayTimeouts {
        open: Duration::from_secs(nar_cfg.storage_open_timeout_secs),
        chunk_read: Duration::from_secs(nar_cfg.send_chunk_timeout_secs),
    };
    let req = RelayRequest {
        job_id,
        store_path,
        resume_from,
        client_token,
    };
    match serve_nar(&state.nar_storage, writer, req, timeouts).await {
        Ok(_) => Ok(()),
        Err(ServeError::NotFound(reason)) => {
            if let Some(hash) = store_hash(store_path) {
                invalidate_cached_path(state, hash, store_path).await;
            }

            Err(anyhow::anyhow!(reason))
        }
        Err(e) => Err(e.into()),
    }
}

/// Purge a `cached_path` row whose NAR is no longer in `nar_storage`.
///
/// Deletes the stale artifact and clears `derivation_output.is_cached` /
/// `cached_path` so the next `CacheQuery` stops claiming the path is available -
/// letting the next build either rebuild from source or pick the path up from a
/// configured upstream. The derivation graph is untouched.
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

/// An unconfirmed row younger than the upload grace is a NAR another instance
/// has staged and not yet uploaded, not a missing one.
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

/// Extract and validate the 32-char store-hash from a `/nix/store/<hash>-name`
/// path. Returns `None` for anything malformed.
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
    use gradient_wire::session::frame::WireMessage;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use tokio::sync::mpsc;

    /// Spy writer: records every message the server attempted to send so the
    /// test can assert exactly which protocol frames were emitted (NarPush,
    /// NarUnavailable, NarAbort, …).
    fn spy_writer(timeout: Duration) -> (ProtoWriter, mpsc::Receiver<Bytes>) {
        ProtoWriter::spy(timeout)
    }

    fn decode(bytes: Bytes) -> ServerMessage {
        ServerMessage::decode(bytes)
            .expect("decode ServerMessage")
            .into_message()
            .expect("deserialise ServerMessage")
    }

    /// Streamed payload arrives as one or more `NarPush` frames whose
    /// concatenated `data` matches the original bytes, with the final frame
    /// flagged `is_final=true`.
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

    /// Missing object → `NarUnavailable` (not `NarAbort`, no NarPush) and an
    /// `Err` from `serve_nar_request`.
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

    /// RAM answers a request the object store could not: the hash is only in
    /// the hot cache.
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

    #[tokio::test]
    async fn a_slot_is_active_until_dropped() {
        let g = gauges();
        let slot = ServeSlot::acquire_with(Arc::new(Semaphore::new(1)), g)
            .await
            .expect("slot");

        assert_eq!(g.serves_active.get(), 1);
        assert_eq!(g.serves_waiting.get(), 0);
        drop(slot);
        assert_eq!(g.serves_active.get(), 0);
    }

    #[tokio::test]
    async fn a_queued_serve_counts_as_waiting() {
        let g = gauges();
        let sem = Arc::new(Semaphore::new(1));
        let held = ServeSlot::acquire_with(Arc::clone(&sem), g)
            .await
            .expect("slot");
        let mut queued = Box::pin(ServeSlot::acquire_with(Arc::clone(&sem), g));
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
    async fn cancelled_wait_releases_waiting() {
        let g = gauges();
        let sem = Arc::new(Semaphore::new(0));
        let waiting = ServeSlot::acquire_with(Arc::clone(&sem), g);
        let cancelled = tokio::time::timeout(Duration::from_millis(20), waiting).await;

        assert!(cancelled.is_err());
        assert_eq!(g.serves_waiting.get(), 0);
        assert_eq!(g.serves_active.get(), 0);
    }

    #[tokio::test]
    async fn a_closed_semaphore_yields_no_slot() {
        let g = gauges();
        let sem = Arc::new(Semaphore::new(0));
        sem.close();

        assert!(ServeSlot::acquire_with(sem, g).await.is_none());
        assert_eq!(g.serves_waiting.get(), 0);
    }
}
