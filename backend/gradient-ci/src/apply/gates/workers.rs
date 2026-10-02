/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::projects::workers::project_has_eval_capable_worker_registration;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ConnectionTrait};

/// The build-dispatch repair pass is stalling `Queued` evaluations only when zero workers are
/// connected. `unpark_no_workers_for_project` is unparking the row once an `eval` registration
/// is active.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn park_if_no_workers<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
    project: ProjectId,
) -> Result<MEvaluation, sea_orm::DbErr> {
    if eval.status != EvaluationStatus::Queued {
        return Ok(eval);
    }
    if project_has_eval_capable_worker_registration(db, project).await? {
        return Ok(eval);
    }
    let mut ae: AEvaluation = eval.into();
    ae.status = Set(EvaluationStatus::Waiting);
    ae.waiting_reason = Set(Some(
        WaitingReason::workers(Vec::new(), 0, Vec::new()).to_json(),
    ));
    ae.updated_at = Set(gradient_types::now());
    ae.update(db).await
}
