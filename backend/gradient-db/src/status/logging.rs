/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::DbContext;
use anyhow::{Context, Result};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter,
};
use tracing::{error, warn};

/// A failure is retried as a pending delivery with the inline copy still in place.
/// Dropping the inline copy is the one best-effort step, because the index is already written.
pub async fn finalize_build_log(
    ctx: &DbContext,
    log_id: gradient_entity::ids::BuildAttemptId,
) -> Result<()> {
    let logs = ctx.storage.log_storage.as_ref();
    let inline = logs
        .read_inline(log_id)
        .await
        .with_context(|| format!("reading the inline build log of attempt {log_id}"))?;
    if inline.is_empty() {
        return Ok(());
    }

    let indexed = indexed_chunk_count(&ctx.worker_db, log_id).await?;
    let earlier = gradient_storage::log_chunk::read_chunks(logs, log_id, indexed)
        .await
        .with_context(|| format!("reading the earlier log chunks of attempt {log_id}"))?;

    let descs = gradient_storage::log_chunk::compress_and_store_chunks(
        logs,
        log_id,
        &(earlier + &inline),
        ctx.config.log.chunk_bytes,
    )
    .await
    .with_context(|| format!("chunking the build log of attempt {log_id}"))?;

    replace_log_chunk_index(&ctx.worker_db, log_id, &descs)
        .await
        .with_context(|| format!("writing the log chunk index of attempt {log_id}"))?;

    if let Err(e) = logs.delete_inline_log(log_id).await {
        warn!(error = %e, build_id = %log_id, "Failed to drop inline log after chunking");
    }

    Ok(())
}

pub async fn enqueue_log_finalize(
    db: &impl ConnectionTrait,
    attempts: impl IntoIterator<Item = gradient_entity::ids::BuildAttemptId>,
) -> Result<(), sea_orm::DbErr> {
    let rows = attempts
        .into_iter()
        .map(|a| (a.to_string(), serde_json::json!({ "attempt": a })))
        .collect();
    crate::deliveries::pending::enqueue_many(
        db,
        crate::deliveries::pending::PendingDeliveryKind::LogFinalize,
        rows,
    )
    .await
}

async fn indexed_chunk_count(
    db: &impl ConnectionTrait,
    log_id: gradient_entity::ids::BuildAttemptId,
) -> Result<u32> {
    use gradient_entity::build_log_chunk::{Column, Entity};
    let count = Entity::find()
        .filter(Column::BuildAttempt.eq(log_id))
        .count(db)
        .await
        .with_context(|| format!("counting the log chunks of attempt {log_id}"))?;
    Ok(count as u32)
}

async fn replace_log_chunk_index(
    db: &impl ConnectionTrait,
    log_id: gradient_entity::ids::BuildAttemptId,
    descs: &[gradient_storage::log_chunk::StoredChunkDesc],
) -> Result<(), sea_orm::DbErr> {
    use gradient_entity::build_log_chunk::{ActiveModel, Column, Entity, Model};
    Entity::delete_many()
        .filter(Column::BuildAttempt.eq(log_id))
        .exec(db)
        .await?;
    if descs.is_empty() {
        return Ok(());
    }
    let rows: Vec<ActiveModel> = descs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            Model {
                id: gradient_entity::ids::BuildLogChunkId::now_v7(),
                build_attempt: log_id,
                chunk_index: i as i32,
                byte_start: d.byte_start as i64,
                byte_len: d.byte_len as i32,
                line_start: d.line_start as i64,
                line_count: d.line_count as i32,
                compressed_size: d.compressed_size as i32,
                color_prefix: d.color_prefix.clone(),
            }
            .into_active_model()
        })
        .collect();
    Entity::insert_many(rows).exec(db).await?;
    Ok(())
}

pub use gradient_entity::phase_event::PhaseSubjectKind;

pub async fn record_phase_event(
    db: &impl ConnectionTrait,
    subject_kind: PhaseSubjectKind,
    subject_id: uuid::Uuid,
    phase: i16,
    worker_id: Option<String>,
    at: chrono::NaiveDateTime,
) -> Result<(), sea_orm::DbErr> {
    let ev = gradient_entity::phase_event::Model {
        id: gradient_entity::ids::PhaseEventId::now_v7(),
        subject_kind,
        subject_id,
        phase,
        at,
        worker_id,
        ..Default::default()
    }
    .into_active_model();
    gradient_entity::phase_event::Entity::insert(ev)
        .exec(db)
        .await?;

    Ok(())
}

pub async fn record_phase_events(
    db: &impl ConnectionTrait,
    subject_kind: PhaseSubjectKind,
    subject_ids: &[uuid::Uuid],
    phase: i16,
    at: chrono::NaiveDateTime,
) -> Result<(), sea_orm::DbErr> {
    // Postgres is capping binds at 65535, and each row is carrying 6 columns.
    const INSERT_CHUNK: usize = 8192;
    let rows: Vec<_> = subject_ids
        .iter()
        .map(|&subject_id| {
            gradient_entity::phase_event::Model {
                id: gradient_entity::ids::PhaseEventId::now_v7(),
                subject_kind,
                subject_id,
                phase,
                at,
                worker_id: None,
                ..Default::default()
            }
            .into_active_model()
        })
        .collect();

    for chunk in rows.chunks(INSERT_CHUNK) {
        gradient_entity::phase_event::Entity::insert_many(chunk.to_vec())
            .exec(db)
            .await?;
    }

    Ok(())
}

pub async fn insert_evaluation_message<C: ConnectionTrait>(
    db: &C,
    evaluation_id: EvaluationId,
    level: MessageLevel,
    message: String,
    source: Option<String>,
) -> Result<(), sea_orm::DbErr> {
    let msg = MEvaluationMessage {
        id: EvaluationMessageId::now_v7(),
        evaluation: evaluation_id,
        level,
        message,
        source,
        created_at: gradient_types::now(),
    }
    .into_active_model();

    EEvaluationMessage::insert(msg).exec(db).await?;
    Ok(())
}

pub async fn insert_evaluation_message_once<C: ConnectionTrait>(
    db: &C,
    evaluation_id: EvaluationId,
    level: MessageLevel,
    message: String,
    source: Option<String>,
) -> Result<(), sea_orm::DbErr> {
    let recorded = EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation_id))
        .filter(CEvaluationMessage::Message.eq(message.as_str()))
        .count(db)
        .await?;
    if recorded > 0 {
        return Ok(());
    }

    insert_evaluation_message(db, evaluation_id, level, message, source).await
}

pub async fn record_evaluation_message(
    ctx: &DbContext,
    evaluation_id: EvaluationId,
    level: MessageLevel,
    message: String,
    source: Option<String>,
) {
    if let Err(e) =
        insert_evaluation_message(&ctx.worker_db, evaluation_id, level, message, source).await
    {
        error!(error = %e, %evaluation_id, "Failed to insert evaluation_message");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::ids::BuildAttemptId;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn finalize_appends_a_late_inline_tail_to_the_earlier_chunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                [BTreeMap::from([("num_items", Value::BigInt(Some(1)))])],
                [BTreeMap::from([("id", Value::from(uuid::Uuid::now_v7()))])],
            ])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let (ctx, _pool) = crate::test_ctx::ctx(db).await;
        let logs = std::sync::Arc::clone(&ctx.storage.log_storage);
        let id = BuildAttemptId::now_v7();
        gradient_storage::log_chunk::compress_and_store_chunks(
            logs.as_ref(),
            id,
            "early\n",
            1 << 16,
        )
        .await
        .unwrap();
        logs.append(id, "late\n").await.unwrap();

        finalize_build_log(&ctx, id).await.unwrap();

        assert_eq!(logs.read(id).await.unwrap(), "early\nlate\n");
        assert!(logs.read_inline(id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn finalize_without_an_inline_log_leaves_the_chunks_and_index_alone() {
        let (ctx, pool) =
            crate::test_ctx::ctx(MockDatabase::new(DatabaseBackend::Postgres).into_connection())
                .await;
        let logs = std::sync::Arc::clone(&ctx.storage.log_storage);
        let id = BuildAttemptId::now_v7();
        gradient_storage::log_chunk::compress_and_store_chunks(
            logs.as_ref(),
            id,
            "done\n",
            1 << 16,
        )
        .await
        .unwrap();

        finalize_build_log(&ctx, id).await.unwrap();

        assert_eq!(logs.read(id).await.unwrap(), "done\n");
        crate::test_ctx::settle(ctx).await;
        assert!(crate::pool::statements(pool.into_transaction_log()).is_empty());
    }
}
