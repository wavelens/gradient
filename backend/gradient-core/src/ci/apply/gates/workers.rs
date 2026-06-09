/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::db::org_has_eval_capable_worker_registration;
use crate::types::waiting_reason::WaitingReason;
use crate::types::*;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ConnectionTrait};

/// Move a freshly-created `Queued` evaluation into `Waiting` with
/// `WaitingReason::Workers { connected_workers: 0, .. }` when the project's
/// organisation has no active worker registration with the `eval` capability
/// gate enabled. Returns the evaluation unchanged when at least one such
/// registration exists.
///
/// Without this gate the eval row would sit in `Queued` indefinitely: the
/// build-dispatch reconciler only stalls Queued evaluations when **zero**
/// workers are connected, not when connected workers all lack `eval`. The
/// row is unparked by `unpark_no_workers_for_org` whenever a worker
/// registration is created or its `enable_eval` / `active` flags flip on.
pub async fn park_if_no_workers<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
    organization: OrganizationId,
) -> Result<MEvaluation, sea_orm::DbErr> {
    if eval.status != EvaluationStatus::Queued {
        return Ok(eval);
    }
    if org_has_eval_capable_worker_registration(db, organization).await? {
        return Ok(eval);
    }
    let mut ae: AEvaluation = eval.into();
    ae.status = Set(EvaluationStatus::Waiting);
    ae.waiting_reason = Set(Some(
        WaitingReason::workers(Vec::new(), 0, Vec::new()).to_json(),
    ));
    ae.updated_at = Set(crate::types::now());
    ae.update(db).await
}
