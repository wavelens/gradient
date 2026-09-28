/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Server-side handlers for the fleet-shared eval-cache transfer (#386).
//!
//! A worker pulls a flake's serialized eval-cache blob by `fingerprint`
//! (presigned-S3 URL or inline chunked stream); pushes go through the upload
//! handshake and are size-guarded here so a stale-small blob never clobbers a
//! larger cached one. Blobs live under `eval-cache/<fingerprint>` in object
//! storage; an `eval_cache_store` row indexes them. Every handler is
//! best-effort: on any error it logs and sends the safe negative response
//! (`Miss`) rather than tearing down the connection.

use gradient_core::ServerState;
use gradient_entity::eval_cache_store;
use gradient_types::ids::EvalCacheStoreId;
use gradient_types::*;
use gradient_wire::types::EvalCachePullOutcome;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use tracing::{debug, warn};

use super::socket::{BULK_CHUNK_SIZE, ProtoWriter, send_server_msg};
use gradient_wire::messages::{PRESIGN_TTL, ServerMessage};

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

    if inline && let Err(e) = stream_blob_inline(state, writer, &job_id, &key).await {
        warn!(%fingerprint, error = %e, "inline eval-cache stream failed");
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub(super) async fn accepts_push(state: &ServerState, fingerprint: &str, size_bytes: u64) -> bool {
    let existing = lookup_row(state, fingerprint).await.map(|r| r.size_bytes);
    should_accept_push(existing, size_bytes)
}

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
pub(super) async fn record_eval_cache(state: &ServerState, fingerprint: &str, size_bytes: u64) {
    upsert_eval_cache_row(state, fingerprint, &storage_key(fingerprint), size_bytes).await;
}

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
/// to `BULK_CHUNK_SIZE` like the NAR pull path. The final frame carries
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
        return Err(anyhow::anyhow!(
            "eval-cache blob {key} vanished before stream"
        ));
    };

    let mut buf: Vec<u8> = Vec::with_capacity(BULK_CHUNK_SIZE);
    let mut offset: u64 = 0;

    while let Some(item) = stream.next().await {
        let bytes = item?;
        let mut slice = &bytes[..];
        while !slice.is_empty() {
            let take = slice.len().min(BULK_CHUNK_SIZE - buf.len());
            buf.extend_from_slice(&slice[..take]);
            slice = &slice[take..];
            if buf.len() == BULK_CHUNK_SIZE {
                let chunk = std::mem::replace(&mut buf, Vec::with_capacity(BULK_CHUNK_SIZE));
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
                    return Err(anyhow::anyhow!(
                        "eval-cache send stalled at offset {offset}"
                    ));
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

/// Stable per-fingerprint stream token for an inline pull.
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

    // ── storage key ─────────────────────────────────────────────────────────────

    #[test]
    fn storage_key_is_namespaced() {
        assert_eq!(storage_key("abc123"), "eval-cache/abc123");
    }
}
