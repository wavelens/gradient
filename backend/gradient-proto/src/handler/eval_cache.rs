/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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

fn storage_key(fingerprint: &str) -> String {
    format!("eval-cache/{fingerprint}")
}

fn should_accept_push(existing: Option<i64>, incoming: u64) -> bool {
    match existing {
        Some(existing) => incoming > existing as u64,
        None => true,
    }
}

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
                        data: chunk.into(),
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
            data: buf.into(),
            offset,
            is_final: true,
        },
    )
    .await
    .map_err(|_| anyhow::anyhow!("eval-cache send stalled on final chunk"))?;

    Ok(())
}

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
}
