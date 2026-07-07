/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::apply::ApprovalInfo;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ConnectionTrait};

/// Move a freshly-created `Queued` evaluation into `Waiting` with
/// `WaitingReason::Approval` when the caller has flagged it as gated. No-op
/// when `info` is `None` or the evaluation is already parked.
pub async fn park_if_pending_approval<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
    info: Option<&ApprovalInfo>,
) -> Result<MEvaluation, sea_orm::DbErr> {
    let Some(info) = info else {
        return Ok(eval);
    };
    if eval.status != EvaluationStatus::Queued {
        return Ok(eval);
    }
    let mut ae: AEvaluation = eval.into();
    ae.status = Set(EvaluationStatus::Waiting);
    ae.waiting_reason = Set(Some(
        WaitingReason::approval(info.pr_number, info.pr_author.clone()).to_json(),
    ));
    ae.updated_at = Set(gradient_types::now());
    ae.update(db).await
}
