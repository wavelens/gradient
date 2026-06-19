/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::DbContext;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter};
use tracing::{error, warn};

/// Compress a finalized build log into zstd chunks, persist the chunk index,
/// and drop the inline copy. Best-effort: failures are logged, never propagated.
pub async fn finalize_build_log(ctx: &DbContext, log_id: gradient_entity::ids::BuildAttemptId) {
    let log_text = ctx
        .storage
        .log_storage
        .read(log_id)
        .await
        .unwrap_or_default();
    if log_text.is_empty() {
        return;
    }
    let descs = match gradient_storage::log_chunk::compress_and_store_chunks(
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
    if let Err(e) = gradient_entity::phase_event::Entity::insert(ev)
        .exec(db)
        .await
    {
        warn!(error = %e, "failed to record phase_event");
    }
}

/// Batch-record the same phase transition for many subjects, one multi-row
/// insert per chunk. Used by bulk status writes (e.g. evaluation abort) instead
/// of one spawned [`record_phase_event`] per subject. Best-effort.
pub async fn record_phase_events(
    db: &impl ConnectionTrait,
    subject_kind: i16,
    subject_ids: &[uuid::Uuid],
    phase: i16,
    at: chrono::NaiveDateTime,
) {
    // Stay well under Postgres' 65535-bind-parameter cap (6 columns per row).
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
        if let Err(e) = gradient_entity::phase_event::Entity::insert_many(chunk.to_vec())
            .exec(db)
            .await
        {
            warn!(error = %e, "failed to record phase_events batch");
            return;
        }
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
