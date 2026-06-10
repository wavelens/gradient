/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::db::org_has_writable_cache;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ConnectionTrait};

/// Move a freshly-created `Queued` evaluation into `Waiting` with
/// `WaitingReason::NoCache` if the project's organisation lacks a writable
/// cache subscription. Returns the evaluation unchanged when at least one
/// ReadWrite/WriteOnly cache is present.
///
/// Callers that go through [`apply_trigger`](super::super::apply_trigger) get
/// this automatically; the manual `/projects/{org}/{project}/evaluate` endpoint
/// applies it directly after calling
/// [`trigger_evaluation`](crate::ci::trigger_evaluation).
pub async fn park_if_no_cache<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
    organization: OrganizationId,
) -> Result<MEvaluation, sea_orm::DbErr> {
    if eval.status != EvaluationStatus::Queued {
        return Ok(eval);
    }
    if org_has_writable_cache(db, organization).await? {
        return Ok(eval);
    }
    let mut ae: AEvaluation = eval.into();
    ae.status = Set(EvaluationStatus::Waiting);
    ae.waiting_reason = Set(Some(WaitingReason::NoCache.to_json()));
    ae.updated_at = Set(gradient_types::now());
    ae.update(db).await
}
