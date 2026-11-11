/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::abort::{AbortKind, abort_evaluation};
use gradient_db::status::EvaluationAbortReasonWithUserIdAndReplacementId;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::triggers::ConcurrencyPolicy;
use gradient_types::*;
use sea_orm::ConnectionTrait;

pub(super) struct ConcurrencyDecision {
    pub concurrent_flag: bool,
    pub aborted_evaluation: Option<EvaluationId>,
    pub hard_abort: bool,
}

pub(super) async fn resolve_concurrency<C: ConnectionTrait>(
    db: &C,
    task: &MTask,
    in_flight: Option<MEvaluation>,
) -> Result<Option<ConcurrencyDecision>, sea_orm::DbErr> {
    let concurrency = task.concurrency;

    let mut aborted_evaluation: Option<EvaluationId> = None;
    let mut hard_abort = false;
    let concurrent_flag = match concurrency {
        ConcurrencyPolicy::All => true,
        ConcurrencyPolicy::Auto => in_flight.is_some(),
        ConcurrencyPolicy::HardAbort | ConcurrencyPolicy::SoftAbort | ConcurrencyPolicy::Skip => {
            false
        }
    };

    if !concurrent_flag && let Some(running) = in_flight {
        let superseded = EvaluationAbortReasonWithUserIdAndReplacementId::superseded(None);
        match concurrency {
            ConcurrencyPolicy::Skip => return Ok(None),
            ConcurrencyPolicy::HardAbort => {
                hard_abort = abort_evaluation(db, running.id, AbortKind::Hard, superseded).await?;
                aborted_evaluation = Some(running.id);
            }
            ConcurrencyPolicy::SoftAbort => {
                abort_evaluation(db, running.id, AbortKind::Soft, superseded).await?;
                aborted_evaluation = Some(running.id);
            }
            ConcurrencyPolicy::All | ConcurrencyPolicy::Auto => {}
        }
    }

    Ok(Some(ConcurrencyDecision {
        concurrent_flag,
        aborted_evaluation,
        hard_abort,
    }))
}

pub(super) fn find_newest_waiting_evaluation_that_newer_trigger_can_replace(
    newest: Option<&MEvaluation>,
) -> Option<EvaluationId> {
    let newest = newest?;
    let replaceable = newest.status == EvaluationStatus::Waiting
        && WaitingReason::from_status_and_json_if_waiting(
            newest.status,
            newest.waiting_reason.as_ref(),
        )
        .is_some_and(|reason| reason.can_be_replaced_by_newer_trigger());
    replaceable.then_some(newest.id)
}

pub(super) async fn abort_superseded_waiting_evaluation<C: ConnectionTrait>(
    db: &C,
    waiting: EvaluationId,
) -> Result<bool, sea_orm::DbErr> {
    let superseded = EvaluationAbortReasonWithUserIdAndReplacementId::superseded(None);
    abort_evaluation(db, waiting, AbortKind::Hard, superseded).await
}
