/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Moving anchors back onto the queue.

use gradient_db::{
    DbContext, emit_transition_effects, unpromote_ungated, update_derivation_build_status,
};
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::debug;

use crate::messages::RequeueScope;

pub(crate) async fn apply(ctx: &DbContext, scope: RequeueScope) -> anyhow::Result<u64> {
    match scope {
        RequeueScope::TransientRetries => transient_retries(ctx).await,
    }
}

/// `FailedTransient` anchors whose exponential backoff window has elapsed go back
/// to `Queued` so the ready-builds pass can dispatch them again. The settle that
/// follows is what makes the requeue legal, not the backoff: an elapsed window says
/// the retry is due, never that the anchor's gates still hold. A dependency a demote
/// or a retire reset to `Created` leaves `unready_deps` above zero, and the dispatch
/// gate trusts `Queued` without re-deriving readiness, so an unsettled requeue
/// dispatches a build against an input nothing can provide.
async fn transient_retries(ctx: &DbContext) -> anyhow::Result<u64> {
    let base = ctx.config.eval.build_retry_backoff_secs;
    let now = gradient_types::now();
    let transient = EDerivationBuild::find()
        .filter(CDerivationBuild::Status.eq(BuildStatus::FailedTransient))
        .all(&ctx.worker_db)
        .await?;
    let mut requeued = Vec::new();
    for anchor in transient {
        if crate::policy::retry_backoff_elapsed(anchor.attempt, anchor.updated_at, now, base) {
            let derivation = anchor.derivation;
            update_derivation_build_status(ctx, anchor, BuildStatus::Queued).await;
            requeued.push(derivation);
        }
    }

    let settled = unpromote_ungated(&ctx.worker_db, &requeued).await?;
    if !settled.is_empty() {
        debug!(
            unpromoted = settled.len(),
            requeued = requeued.len(),
            "retried anchors whose gates no longer hold left the queue again"
        );
    }

    emit_transition_effects(ctx, &settled).await;

    Ok(requeued.len() as u64)
}
