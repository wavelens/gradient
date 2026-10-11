/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::apply::ApprovalInfo;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;
use sea_orm::ConnectionTrait;

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
    let reason = WaitingReason::approval(info.pr_number, info.pr_author.clone());
    super::park(db, eval, reason).await
}
