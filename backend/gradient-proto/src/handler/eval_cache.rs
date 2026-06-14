/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Server-side handlers for the fleet-shared eval-cache transfer (#386).
//!
//! Mirrors the NAR transfer: a worker pulls a flake's serialized eval-cache
//! blob by `fingerprint` (presigned-S3 URL or inline chunked stream) and pushes
//! an updated one back, size-guarded so a stale-small blob never clobbers a
//! larger cached one. Blobs live under `eval-cache/<fingerprint>` in object
//! storage; an `eval_cache_store` row indexes them. Every handler is
//! best-effort: on any error it logs and sends the safe negative response
//! (`Miss` / `Skip`) rather than tearing down the connection.

use std::collections::HashMap;
use std::time::Duration;

use gradient_entity::eval_cache_store;
use gradient_types::ids::EvalCacheStoreId;
use gradient_types::proto::{EvalCachePullOutcome, EvalCachePushMode};
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use tracing::{debug, warn};

use super::socket::{NAR_PUSH_CHUNK_SIZE, ProtoWriter, send_server_msg};
use crate::messages::ServerMessage;

/// Presigned-URL / inline-stream validity window. Matches the NAR cache TTL.
const PRESIGN_TTL: Duration = Duration::from_secs(3600);

/// Storage key for a fingerprint's eval-cache blob. Kept here (not just in
/// `NarStore`) so the convention is visible at the call site and unit-testable.
fn storage_key(fingerprint: &str) -> String {
    format!("eval-cache/{fingerprint}")
}

// ── Pure decisions (unit-tested without a live store/DB) ──────────────────────

/// Whether an incoming push should be stored. Accept when there is no existing
/// row or the incoming blob is strictly larger; otherwise skip (the size-guard
/// that prevents a stale-small overwrite).
fn should_accept_push(existing: Option<i64>, incoming: u64) -> bool {
    match existing {
        Some(existing) => incoming > existing as u64,
        None => true,
    }
}

/// Pick the pull outcome from `(row, presigned_url)`: `Miss` when no row, a
/// presigned GET when the store minted one (S3), else an inline stream header.
fn pull_outcome(
    row: Option<&eval_cache_store::Model>,
    presigned_url: Option<String>,
    stream_token: impl FnOnce() -> String,
) -> EvalCachePullOutcome {
    match row {
        None => EvalCachePullOutcome::Miss,
        Some(row) => match presigned_url {
            Some(url) => EvalCachePullOutcome::Presigned { url },
            None => EvalCachePullOutcome::Inline {
                total_bytes: row.size_bytes.max(0) as u64,
                stream_token: stream_token(),
            },
        },
    }
}

/// Pick the push grant mode from `(existing_row, incoming_size, presigned_url)`:
/// `Skip` when the size-guard rejects the upload, a presigned PUT when the store
/// minted one (S3), else an inline upload.
fn push_mode(
    existing: Option<i64>,
    incoming: u64,
    presigned_url: Option<String>,
    stream_token: impl FnOnce() -> String,
) -> EvalCachePushMode {
    if !should_accept_push(existing, incoming) {
        return EvalCachePushMode::Skip;
    }

    match presigned_url {
        Some(url) => EvalCachePushMode::Presigned { url },
        None => EvalCachePushMode::Inline {
            stream_token: stream_token(),
        },
    }
}

// ── Inline upload staging (local-FS fallback) ─────────────────────────────────

#[derive(Default)]
struct StagedBlob {
    bytes: Vec<u8>,
}

/// Per-session in-memory staging for inline eval-cache pushes, keyed by
/// fingerprint. Eval-cache blobs are small SQLite files, so unlike the
/// disk-backed `NarReceiveStore` this stays in RAM; a per-session byte budget
/// (shared with the NAR partial budget) bounds a rogue worker.
pub(super) struct EvalCacheReceiveStore {
    max_bytes: u64,
    active: HashMap<String, StagedBlob>,
}

impl EvalCacheReceiveStore {
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            active: HashMap::new(),
        }
    }

    fn open(&mut self, fingerprint: &str) {
        self.active.insert(fingerprint.to_owned(), StagedBlob::default());
    }

    /// Append a contiguous chunk for `fingerprint`. Returns `false` (and drops
    /// the staging) on a non-contiguous offset or a budget overrun.
    fn append(&mut self, fingerprint: &str, offset: u64, data: &[u8]) -> bool {
        let total: u64 = self.active.values().map(|b| b.bytes.len() as u64).sum();
        let Some(blob) = self.active.get_mut(fingerprint) else {
            return false;
        };

        if offset != blob.bytes.len() as u64
            || total.saturating_add(data.len() as u64) > self.max_bytes
        {
            self.active.remove(fingerprint);
            return false;
        }

        blob.bytes.extend_from_slice(data);
        true
    }

    fn finish(&mut self, fingerprint: &str) -> Option<Vec<u8>> {
        self.active.remove(fingerprint).map(|b| b.bytes)
    }
}

// ── Async handlers ────────────────────────────────────────────────────────────

/// `EvalCachePull`: serve the blob for `fingerprint` (presigned URL, inline
/// stream, or `Miss`).
pub(super) async fn handle_eval_cache_pull(
    state: &ServerState,
    writer: &ProtoWriter,
    job_id: String,
    fingerprint: String,
) {
    let row = lookup_row(state, &fingerprint).await;
    let key = storage_key(&fingerprint);

    let presigned = match &row {
        Some(_) => state
            .nar_storage
            .presigned_eval_cache_get_url(&fingerprint, PRESIGN_TTL)
            .await
            .unwrap_or_else(|e| {
                warn!(%fingerprint, error = %e, "presigned eval-cache GET failed; falling back to inline");
                None
            }),
        None => None,
    };

    let token = stream_token(&fingerprint);
    let outcome = pull_outcome(row.as_ref(), presigned, || token.clone());
    let inline = matches!(outcome, EvalCachePullOutcome::Inline { .. });

    let _ = send_server_msg(
        writer,
        &ServerMessage::EvalCachePullResult {
            job_id: job_id.clone(),
            outcome,
        },
    )
    .await;

    if inline {
        if let Err(e) = stream_blob_inline(state, writer, &job_id, &key).await {
            warn!(%fingerprint, error = %e, "inline eval-cache stream failed");
        }
    }
}

/// `EvalCachePush`: grant a presigned PUT, an inline upload, or `Skip` (the
/// size-guard rejected a stale-small overwrite).
pub(super) async fn handle_eval_cache_push(
    state: &ServerState,
    writer: &ProtoWriter,
    eval_cache: &mut EvalCacheReceiveStore,
    job_id: String,
    fingerprint: String,
    size_bytes: u64,
) {
    let existing = lookup_row(state, &fingerprint).await.map(|r| r.size_bytes);

    let presigned = if should_accept_push(existing, size_bytes) {
        state
            .nar_storage
            .presigned_eval_cache_put_url(&fingerprint, PRESIGN_TTL)
            .await
            .unwrap_or_else(|e| {
                warn!(%fingerprint, error = %e, "presigned eval-cache PUT failed; falling back to inline");
                None
            })
    } else {
        None
    };

    let token = stream_token(&fingerprint);
    let mode = push_mode(existing, size_bytes, presigned, || token.clone());

    if let EvalCachePushMode::Inline { .. } = &mode {
        eval_cache.open(&fingerprint);
    }

    let _ = send_server_msg(
        writer,
        &ServerMessage::EvalCachePushGrant { job_id, mode },
    )
    .await;
}

/// `EvalCacheChunk` (worker→server, inline push body): stage the chunk and, on
/// the final one, commit the assembled blob and upsert the row.
pub(super) async fn handle_eval_cache_chunk(
    state: &ServerState,
    eval_cache: &mut EvalCacheReceiveStore,
    job_id: &str,
    data: Vec<u8>,
    offset: u64,
    is_final: bool,
) {
    let Some(fingerprint) = fingerprint_for_chunk(eval_cache) else {
        debug!(%job_id, "EvalCacheChunk for unknown stream; dropping");
        return;
    };

    if !eval_cache.append(&fingerprint, offset, &data) {
        warn!(%job_id, %fingerprint, offset, "eval-cache chunk rejected (non-contiguous or over budget)");
        return;
    }

    if !is_final {
        return;
    }

    let Some(bytes) = eval_cache.finish(&fingerprint) else {
        return;
    };
    let size_bytes = bytes.len() as u64;
    let key = storage_key(&fingerprint);
    if let Err(e) = state.nar_storage.put_eval_cache(&fingerprint, bytes).await {
        warn!(%fingerprint, error = %e, "failed to write eval-cache blob to storage");
        return;
    }

    upsert_eval_cache_row(state, &fingerprint, &key, size_bytes).await;
}

/// `EvalCachePushDone` (after a presigned PUT): upsert the row for the
/// now-uploaded blob.
pub(super) async fn handle_eval_cache_push_done(
    state: &ServerState,
    fingerprint: String,
    size_bytes: u64,
) {
    let key = storage_key(&fingerprint);
    upsert_eval_cache_row(state, &fingerprint, &key, size_bytes).await;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn lookup_row(state: &ServerState, fingerprint: &str) -> Option<eval_cache_store::Model> {
    match EEvalCacheStore::find()
        .filter(eval_cache_store::Column::Fingerprint.eq(fingerprint))
        .one(&state.worker_db)
        .await
    {
        Ok(row) => row,
        Err(e) => {
            warn!(%fingerprint, error = %e, "eval_cache_store lookup failed");
            None
        }
    }
}

/// Upsert by the unique `fingerprint` index. The size-guard lives in
/// [`should_accept_push`] (checked before granting the upload), so this always
/// records the freshly-stored blob; on conflict it refreshes `storage_path`,
/// `size_bytes`, and `updated_at`.
async fn upsert_eval_cache_row(
    state: &ServerState,
    fingerprint: &str,
    storage_path: &str,
    size_bytes: u64,
) {
    let now = gradient_types::now();
    let model = MEvalCacheStore {
        id: EvalCacheStoreId::now_v7(),
        fingerprint: fingerprint.to_owned(),
        storage_path: storage_path.to_owned(),
        size_bytes: size_bytes as i64,
        created_at: now,
        updated_at: now,
    };

    let result = EEvalCacheStore::insert(model.into_active_model())
        .on_conflict(
            OnConflict::column(eval_cache_store::Column::Fingerprint)
                .update_columns([
                    eval_cache_store::Column::StoragePath,
                    eval_cache_store::Column::SizeBytes,
                    eval_cache_store::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec(&state.worker_db)
        .await;
    if let Err(e) = result {
        warn!(%fingerprint, error = %e, "eval_cache_store upsert failed");
    } else {
        debug!(%fingerprint, size_bytes, "eval_cache_store upserted");
    }
}

/// Stream a stored eval-cache blob inline as `EvalCacheChunk` frames, coalesced
/// to `NAR_PUSH_CHUNK_SIZE` like the NAR pull path. The final frame carries
/// `is_final = true`.
async fn stream_blob_inline(
    state: &ServerState,
    writer: &ProtoWriter,
    job_id: &str,
    key: &str,
) -> anyhow::Result<()> {
    use futures::StreamExt as _;

    let fingerprint = key.strip_prefix("eval-cache/").unwrap_or(key);
    let Some((_size, mut stream)) = state.nar_storage.get_eval_cache_stream(fingerprint).await?
    else {
        return Err(anyhow::anyhow!("eval-cache blob {key} vanished before stream"));
    };

    let mut buf: Vec<u8> = Vec::with_capacity(NAR_PUSH_CHUNK_SIZE);
    let mut offset: u64 = 0;

    while let Some(item) = stream.next().await {
        let bytes = item?;
        let mut slice = &bytes[..];
        while !slice.is_empty() {
            let take = slice.len().min(NAR_PUSH_CHUNK_SIZE - buf.len());
            buf.extend_from_slice(&slice[..take]);
            slice = &slice[take..];
            if buf.len() == NAR_PUSH_CHUNK_SIZE {
                let chunk = std::mem::replace(&mut buf, Vec::with_capacity(NAR_PUSH_CHUNK_SIZE));
                let len = chunk.len() as u64;
                if send_server_msg(
                    writer,
                    &ServerMessage::EvalCacheChunk {
                        job_id: job_id.to_owned(),
                        data: chunk,
                        offset,
                        is_final: false,
                    },
                )
                .await
                .is_err()
                {
                    return Err(anyhow::anyhow!("eval-cache send stalled at offset {offset}"));
                }

                offset += len;
            }
        }
    }

    send_server_msg(
        writer,
        &ServerMessage::EvalCacheChunk {
            job_id: job_id.to_owned(),
            data: buf,
            offset,
            is_final: true,
        },
    )
    .await
    .map_err(|_| anyhow::anyhow!("eval-cache send stalled on final chunk"))?;

    Ok(())
}

/// Inline pushes carry only `job_id` on each chunk, so the active fingerprint is
/// the one staged by the preceding `EvalCachePush` grant. A worker uploads one
/// blob at a time per session, so the single active key identifies the stream.
fn fingerprint_for_chunk(eval_cache: &EvalCacheReceiveStore) -> Option<String> {
    let mut keys = eval_cache.active.keys();
    let first = keys.next()?.clone();
    keys.next().is_none().then_some(first)
}

/// Stable per-fingerprint stream token. Deterministic so a reconnecting worker's
/// resume attempt validates against the same value (mirrors the NAR `len-<n>`
/// token shape).
fn stream_token(fingerprint: &str) -> String {
    format!("ec-{fingerprint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(size: i64) -> eval_cache_store::Model {
        eval_cache_store::Model {
            id: EvalCacheStoreId::now_v7(),
            fingerprint: "fp".into(),
            storage_path: storage_key("fp"),
            size_bytes: size,
            created_at: gradient_types::now(),
            updated_at: gradient_types::now(),
        }
    }

    // ── size-guard ────────────────────────────────────────────────────────────

    #[test]
    fn accept_when_no_existing_row() {
        assert!(should_accept_push(None, 0));
        assert!(should_accept_push(None, 1024));
    }

    #[test]
    fn accept_when_incoming_strictly_larger() {
        assert!(should_accept_push(Some(100), 101));
    }

    #[test]
    fn skip_when_incoming_equal_or_smaller() {
        assert!(!should_accept_push(Some(100), 100));
        assert!(!should_accept_push(Some(100), 99));
    }

    // ── pull-outcome selection ──────────────────────────────────────────────────

    #[test]
    fn pull_miss_when_no_row() {
        let outcome = pull_outcome(None, Some("https://s3/x".into()), || "tok".into());
        assert_eq!(outcome, EvalCachePullOutcome::Miss);
    }

    #[test]
    fn pull_presigned_when_url_present() {
        let r = row(42);
        let outcome = pull_outcome(Some(&r), Some("https://s3/x".into()), || "tok".into());
        assert_eq!(
            outcome,
            EvalCachePullOutcome::Presigned {
                url: "https://s3/x".into()
            }
        );
    }

    #[test]
    fn pull_inline_when_no_url() {
        let r = row(42);
        let outcome = pull_outcome(Some(&r), None, || "tok".into());
        assert_eq!(
            outcome,
            EvalCachePullOutcome::Inline {
                total_bytes: 42,
                stream_token: "tok".into()
            }
        );
    }

    // ── push-mode selection ─────────────────────────────────────────────────────

    #[test]
    fn push_skip_when_guard_rejects() {
        let mode = push_mode(Some(100), 100, Some("https://s3/x".into()), || "tok".into());
        assert_eq!(mode, EvalCachePushMode::Skip);
    }

    #[test]
    fn push_presigned_when_accepted_and_url_present() {
        let mode = push_mode(Some(100), 200, Some("https://s3/x".into()), || "tok".into());
        assert_eq!(
            mode,
            EvalCachePushMode::Presigned {
                url: "https://s3/x".into()
            }
        );
    }

    #[test]
    fn push_inline_when_accepted_and_no_url() {
        let mode = push_mode(None, 200, None, || "tok".into());
        assert_eq!(
            mode,
            EvalCachePushMode::Inline {
                stream_token: "tok".into()
            }
        );
    }

    // ── storage key + token ─────────────────────────────────────────────────────

    #[test]
    fn storage_key_is_namespaced() {
        assert_eq!(storage_key("abc123"), "eval-cache/abc123");
    }

    #[test]
    fn stream_token_is_deterministic() {
        assert_eq!(stream_token("fp"), stream_token("fp"));
        assert_ne!(stream_token("fp"), stream_token("fp2"));
    }

    // ── inline staging ──────────────────────────────────────────────────────────

    #[test]
    fn staging_appends_contiguous_and_finishes() {
        let mut s = EvalCacheReceiveStore::new(1024);
        s.open("fp");
        assert!(s.append("fp", 0, &[1, 2, 3]));
        assert!(s.append("fp", 3, &[4, 5]));
        assert_eq!(s.finish("fp"), Some(vec![1, 2, 3, 4, 5]));
    }

    #[test]
    fn staging_rejects_non_contiguous_offset() {
        let mut s = EvalCacheReceiveStore::new(1024);
        s.open("fp");
        assert!(s.append("fp", 0, &[1, 2, 3]));
        assert!(!s.append("fp", 99, &[4]));
        assert!(s.finish("fp").is_none());
    }

    #[test]
    fn staging_rejects_over_budget() {
        let mut s = EvalCacheReceiveStore::new(4);
        s.open("fp");
        assert!(!s.append("fp", 0, &[0u8; 5]));
    }

    #[test]
    fn staging_chunk_needs_open_stream() {
        let mut s = EvalCacheReceiveStore::new(1024);
        assert!(!s.append("fp", 0, &[1]));
    }

    #[test]
    fn fingerprint_for_chunk_resolves_single_active() {
        let mut s = EvalCacheReceiveStore::new(1024);
        assert!(fingerprint_for_chunk(&s).is_none());
        s.open("fp");
        assert_eq!(fingerprint_for_chunk(&s).as_deref(), Some("fp"));
        s.open("fp2");
        assert!(fingerprint_for_chunk(&s).is_none(), "ambiguous when 2 active");
    }
}
