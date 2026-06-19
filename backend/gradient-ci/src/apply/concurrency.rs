/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::abort::{AbortKind, abort_evaluation};
use gradient_types::triggers::ConcurrencyPolicy;
use gradient_types::*;
use sea_orm::ConnectionTrait;

/// Outcome of applying the project's [`ConcurrencyPolicy`] to the in-flight
/// evaluation: whether the new run is allowed to be concurrent, plus any
/// evaluation/builds the policy aborted to make room.
pub(super) struct ConcurrencyDecision {
    pub concurrent_flag: bool,
    pub aborted_evaluation: Option<EvaluationId>,
    pub aborted_anchors: Vec<DerivationBuildId>,
}

/// Applies the project's concurrency policy against `in_flight`. Returns `None`
/// when the `Skip` policy says to drop this trigger entirely; otherwise a
/// [`ConcurrencyDecision`] describing the abort side-effects and the
/// `concurrent` flag to pass through to `trigger_evaluation`.
pub(super) async fn resolve_concurrency<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    in_flight: Option<MEvaluation>,
) -> Result<Option<ConcurrencyDecision>, sea_orm::DbErr> {
    let concurrency =
        ConcurrencyPolicy::try_from(project.concurrency).unwrap_or(ConcurrencyPolicy::SoftAbort);

    let mut aborted_evaluation: Option<EvaluationId> = None;
    let mut aborted_anchors: Vec<DerivationBuildId> = Vec::new();
    let concurrent_flag = matches!(concurrency, ConcurrencyPolicy::All);

    if !concurrent_flag && let Some(running) = in_flight {
        match concurrency {
            ConcurrencyPolicy::Skip => return Ok(None),
            ConcurrencyPolicy::HardAbort => {
                aborted_anchors = abort_evaluation(db, running.id, AbortKind::Hard).await?;
                aborted_evaluation = Some(running.id);
            }
            ConcurrencyPolicy::SoftAbort => {
                abort_evaluation(db, running.id, AbortKind::Soft).await?;
                aborted_evaluation = Some(running.id);
            }
            // Excluded by the `!concurrent_flag` guard; `All` allows concurrent runs, so no abort.
            ConcurrencyPolicy::All => {}
        }
    }

    Ok(Some(ConcurrencyDecision {
        concurrent_flag,
        aborted_evaluation,
        aborted_anchors,
    }))
}
