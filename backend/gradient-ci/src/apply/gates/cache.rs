/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::projects::caches::project_has_writable_cache;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;
use sea_orm::ConnectionTrait;

/// The manual `/tasks/{project}/{task}/evaluate` endpoint is applying this gate itself after
/// `trigger_evaluation`.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn park_if_no_cache<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
    project: ProjectId,
) -> Result<MEvaluation, sea_orm::DbErr> {
    if eval.status != EvaluationStatus::Queued {
        return Ok(eval);
    }
    if project_has_writable_cache(db, project).await? {
        return Ok(eval);
    }
    super::park(db, eval, WaitingReason::NoCache).await
}
