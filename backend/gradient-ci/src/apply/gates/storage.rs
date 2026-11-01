/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;
use sea_orm::ConnectionTrait;

#[tracing::instrument(level = "debug", skip_all)]
pub async fn park_storage_full<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
) -> Result<MEvaluation, sea_orm::DbErr> {
    if eval.status != EvaluationStatus::Queued {
        return Ok(eval);
    }
    super::park(db, eval, WaitingReason::CacheStorageFull).await
}
