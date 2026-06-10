/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::db::DbContext;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use tracing::{error, warn};

/// Compress a finalized build log into zstd chunks, persist the chunk index,
/// and drop the inline copy. Best-effort: failures are logged, never propagated.
pub async fn finalize_build_log(ctx: &DbContext, log_id: gradient_entity::ids::BuildId) {
    let log_text = ctx
        .storage
        .log_storage
        .read(log_id)
        .await
        .unwrap_or_default();
    if log_text.is_empty() {
        return;
    }
    let descs = match crate::storage::log_chunk::compress_and_store_chunks(
        ctx.storage.log_storage.as_ref(),
        log_id,
        &log_text,
        ctx.config.storage.log_chunk_bytes,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            error!(error = %e, build_id = %log_id, "Failed to chunk build log");
            return;
        }
    };
    if let Err(e) = replace_log_chunk_index(&ctx.worker_db, log_id, &descs).await {
        error!(error = %e, build_id = %log_id, "Failed to write log chunk index");
        return;
    }
    if let Err(e) = ctx.storage.log_storage.delete_inline_log(log_id).await {
        warn!(error = %e, build_id = %log_id, "Failed to drop inline log after chunking");
    }
}

/// Replace the `build_log_chunk` rows for `log_id` with `descs` (idempotent).
async fn replace_log_chunk_index(
    db: &impl ConnectionTrait,
    log_id: gradient_entity::ids::BuildId,
    descs: &[crate::storage::log_chunk::StoredChunkDesc],
) -> Result<(), sea_orm::DbErr> {
    use gradient_entity::build_log_chunk::{ActiveModel, Column, Entity};
    Entity::delete_many()
        .filter(Column::Build.eq(log_id))
        .exec(db)
        .await?;
    if descs.is_empty() {
        return Ok(());
    }
    let rows: Vec<ActiveModel> = descs
        .iter()
        .enumerate()
        .map(|(i, d)| ActiveModel {
            id: Set(gradient_entity::ids::BuildLogChunkId::now_v7()),
            build: Set(log_id),
            chunk_index: Set(i as i32),
            byte_start: Set(d.byte_start as i64),
            byte_len: Set(d.byte_len as i32),
            line_start: Set(d.line_start as i64),
            line_count: Set(d.line_count as i32),
            compressed_size: Set(d.compressed_size as i32),
            color_prefix: Set(d.color_prefix.clone()),
        })
        .collect();
    Entity::insert_many(rows).exec(db).await?;
    Ok(())
}

pub const PHASE_SUBJECT_BUILD: i16 = 0;
pub const PHASE_SUBJECT_EVALUATION: i16 = 1;

/// Append-only record of a build/evaluation phase transition. Best-effort:
/// failures are logged, never propagated, so instrumentation can't break a
/// status transition.
pub async fn record_phase_event(
    db: &impl ConnectionTrait,
    subject_kind: i16,
    subject_id: uuid::Uuid,
    phase: i16,
    worker_id: Option<String>,
    at: chrono::NaiveDateTime,
) {
    let ev = gradient_entity::phase_event::ActiveModel {
        id: Set(gradient_entity::ids::PhaseEventId::now_v7()),
        subject_kind: Set(subject_kind),
        subject_id: Set(subject_id),
        phase: Set(phase),
        event: Set(0),
        at: Set(at),
        worker_id: Set(worker_id),
        detail: Set(None),
    };
    if let Err(e) = gradient_entity::phase_event::Entity::insert(ev)
        .exec(db)
        .await
    {
        warn!(error = %e, "failed to record phase_event");
    }
}

/// Inserts a single `evaluation_message` row, propagating any DB error.
pub async fn insert_evaluation_message<C: ConnectionTrait>(
    db: &C,
    evaluation_id: EvaluationId,
    level: MessageLevel,
    message: String,
    source: Option<String>,
) -> Result<(), sea_orm::DbErr> {
    let msg = AEvaluationMessage {
        id: Set(EvaluationMessageId::now_v7()),
        evaluation: Set(evaluation_id),
        level: Set(level),
        message: Set(message),
        source: Set(source),
        created_at: Set(gradient_types::now()),
    };
    EEvaluationMessage::insert(msg).exec(db).await?;
    Ok(())
}

/// Inserts a single `evaluation_message` row without changing the evaluation status.
///
/// Use for partial failures (e.g. one attr path failed to evaluate) where the
/// evaluation as a whole continues.
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
