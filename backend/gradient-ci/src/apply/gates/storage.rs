/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::caches::capacity::project_caches_all_full;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;
use sea_orm::ConnectionTrait;

/// A project without writable caches is left to [`park_if_no_cache`](super::park_if_no_cache).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn park_if_storage_full<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
    project: ProjectId,
    instance_max_storage_gb: u32,
) -> Result<MEvaluation, sea_orm::DbErr> {
    if eval.status != EvaluationStatus::Queued {
        return Ok(eval);
    }
    if !project_caches_all_full(db, project, instance_max_storage_gb).await? {
        return Ok(eval);
    }
    super::park(db, eval, WaitingReason::CacheStorageFull).await
}
