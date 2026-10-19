/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::WorkerDb;
use gradient_entity::eval_cache_store;
use gradient_storage::NarStore;
use gradient_types::ids::{CacheId, EvalCacheStoreId};
use gradient_types::*;
use gradient_wire::types::EvalCachePullOutcome;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use tracing::{debug, warn};

use super::socket::{BULK_CHUNK_SIZE, ProtoWriter, send_server_msg};
use gradient_wire::messages::{PRESIGN_TTL, ServerMessage};

pub(super) fn eval_cache_key(cache: CacheId, fingerprint: &str) -> String {
    format!("eval-cache/{cache}/{fingerprint}")
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
    db: &WorkerDb,
    nar_storage: &NarStore,
    writer: &ProtoWriter,
    readable: &[CacheId],
    job_id: String,
    fingerprint: String,
) {
    let row = newest_row(db, readable, &fingerprint).await;

    let presigned = match &row {
        Some(row) => nar_storage
            .presigned_eval_cache_get_url(&row.storage_path, PRESIGN_TTL)
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

    if inline
        && let Some(row) = row
        && let Err(e) = stream_blob_inline(nar_storage, writer, &job_id, &row.storage_path).await
    {
        warn!(%fingerprint, error = %e, "inline eval-cache stream failed");
    }
}

pub(super) async fn accepts_push(
    db: &WorkerDb,
    writable: &[CacheId],
    fingerprint: &str,
    size_bytes: u64,
) -> bool {
    let rows = rows_of(db, writable, fingerprint).await;
    let existing = (rows.len() == writable.len())
        .then(|| rows.iter().map(|r| r.size_bytes).min())
        .flatten();
    should_accept_push(existing, size_bytes)
}

async fn newest_row(
    db: &WorkerDb,
    caches: &[CacheId],
    fingerprint: &str,
) -> Option<eval_cache_store::Model> {
    rows_of(db, caches, fingerprint)
        .await
        .into_iter()
        .max_by_key(|r| r.updated_at)
}

async fn rows_of(
    db: &WorkerDb,
    caches: &[CacheId],
    fingerprint: &str,
) -> Vec<eval_cache_store::Model> {
    if caches.is_empty() {
        return Vec::new();
    }
    EEvalCacheStore::find()
        .filter(eval_cache_store::Column::Fingerprint.eq(fingerprint))
        .filter(eval_cache_store::Column::Cache.is_in(caches.iter().copied()))
        .all(db)
        .await
        .unwrap_or_else(|e| {
            warn!(%fingerprint, error = %e, "eval_cache_store lookup failed");
            Vec::new()
        })
}

pub(super) async fn record_eval_cache(
    db: &WorkerDb,
    nar_storage: &NarStore,
    writable: &[CacheId],
    fingerprint: &str,
    size_bytes: u64,
) {
    let Some((&uploaded, copies)) = writable.split_first() else {
        return;
    };
    let uploaded_key = eval_cache_key(uploaded, fingerprint);
    upsert_eval_cache_row(db, uploaded, fingerprint, &uploaded_key, size_bytes).await;
    for &cache in copies {
        let key = eval_cache_key(cache, fingerprint);
        match nar_storage.copy_eval_cache(&uploaded_key, &key).await {
            Ok(()) => upsert_eval_cache_row(db, cache, fingerprint, &key, size_bytes).await,
            Err(e) => warn!(%fingerprint, %cache, error = %e, "eval-cache copy failed"),
        }
    }
}

async fn upsert_eval_cache_row(
    db: &WorkerDb,
    cache: CacheId,
    fingerprint: &str,
    storage_path: &str,
    size_bytes: u64,
) {
    let now = gradient_types::now();
    let model = MEvalCacheStore {
        id: EvalCacheStoreId::now_v7(),
        cache,
        fingerprint: fingerprint.to_owned(),
        storage_path: storage_path.to_owned(),
        size_bytes: size_bytes as i64,
        created_at: now,
        updated_at: now,
    };

    let result = EEvalCacheStore::insert(model.into_active_model())
        .on_conflict(
            OnConflict::columns([
                eval_cache_store::Column::Cache,
                eval_cache_store::Column::Fingerprint,
            ])
            .update_columns([
                eval_cache_store::Column::StoragePath,
                eval_cache_store::Column::SizeBytes,
                eval_cache_store::Column::UpdatedAt,
            ])
            .to_owned(),
        )
        .exec_without_returning(db)
        .await;
    if let Err(e) = result {
        warn!(%fingerprint, %cache, error = %e, "eval_cache_store upsert failed");
    } else {
        debug!(%fingerprint, %cache, size_bytes, "eval_cache_store upserted");
    }
}

async fn stream_blob_inline(
    nar_storage: &NarStore,
    writer: &ProtoWriter,
    job_id: &str,
    key: &str,
) -> anyhow::Result<()> {
    use futures::StreamExt as _;

    let Some((_size, mut stream)) = nar_storage.get_eval_cache_stream(key).await? else {
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
    use gradient_fixtures::mock::{exec_ok, mock_db, raw_statements};

    fn row(size: i64) -> eval_cache_store::Model {
        eval_cache_store::Model {
            id: EvalCacheStoreId::now_v7(),
            cache: CacheId::nil(),
            fingerprint: "fp".into(),
            storage_path: eval_cache_key(CacheId::nil(), "fp"),
            size_bytes: size,
            created_at: gradient_types::now(),
            updated_at: gradient_types::now(),
        }
    }

    #[test]
    fn should_accept_push_keeps_the_largest_blob() {
        assert!(should_accept_push(None, 0));
        assert!(should_accept_push(None, 1024));
        assert!(should_accept_push(Some(100), 101));
        assert!(!should_accept_push(Some(100), 100));
        assert!(!should_accept_push(Some(100), 99));
    }

    #[test]
    fn pull_outcome_prefers_a_presigned_url_then_inlines() {
        assert_eq!(
            pull_outcome(None, Some("https://s3/x".into()), || "tok".into()),
            EvalCachePullOutcome::Miss
        );

        let r = row(42);
        assert_eq!(
            pull_outcome(Some(&r), Some("https://s3/x".into()), || "tok".into()),
            EvalCachePullOutcome::Presigned {
                url: "https://s3/x".into()
            }
        );
        assert_eq!(
            pull_outcome(Some(&r), None, || "tok".into()),
            EvalCachePullOutcome::Inline {
                total_bytes: 42,
                stream_token: "tok".into()
            }
        );
    }

    #[tokio::test]
    async fn a_push_is_recorded_for_every_writable_cache() {
        let (first, second) = (CacheId::now_v7(), CacheId::now_v7());
        let dir = tempfile::TempDir::new().unwrap();
        let store = NarStore::local(dir.path()).unwrap();
        store
            .put_eval_cache(&eval_cache_key(first, "fp"), b"blob".to_vec())
            .await
            .unwrap();
        let db = WorkerDb::new(
            mock_db()
                .append_exec_results([exec_ok(), exec_ok()])
                .into_connection(),
        );

        record_eval_cache(&db, &store, &[first, second], "fp", 10).await;

        assert_eq!(
            store
                .get_eval_cache(&eval_cache_key(second, "fp"))
                .await
                .unwrap(),
            Some(b"blob".to_vec())
        );
        let inserts = raw_statements(db.into_transaction_log());
        assert_eq!(inserts.len(), 2, "{inserts:?}");
        for (insert, cache) in inserts.iter().zip([first, second]) {
            assert!(
                insert
                    .sql
                    .contains(r#"ON CONFLICT ("cache", "fingerprint")"#),
                "{}",
                insert.sql
            );
            let values = format!("{:?}", insert.values);
            assert!(
                values.contains(&format!("{:?}", eval_cache_key(cache, "fp"))),
                "{values}"
            );
        }
    }

    #[tokio::test]
    async fn a_project_without_readable_caches_misses_without_a_lookup() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = NarStore::local(dir.path()).unwrap();
        let db = WorkerDb::new(mock_db().into_connection());
        let (writer, mut sent) = ProtoWriter::spy(std::time::Duration::from_secs(5));

        handle_eval_cache_pull(&db, &store, &writer, &[], "j1".into(), "fp".into()).await;

        let reply = gradient_wire::codec::from_bytes::<ServerMessage>(
            sent.try_recv().unwrap(),
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .unwrap();
        assert!(matches!(
            reply,
            ServerMessage::EvalCachePullResult {
                outcome: EvalCachePullOutcome::Miss,
                ..
            }
        ));
        assert!(db.into_transaction_log().is_empty());
    }
}
