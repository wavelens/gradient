/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Moving anchors back onto the queue.

use gradient_db::{DbContext, update_derivation_build_status};
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::messages::RequeueScope;

pub(crate) async fn apply(ctx: &DbContext, scope: RequeueScope) -> anyhow::Result<u64> {
    match scope {
        RequeueScope::TransientRetries => transient_retries(ctx).await,
    }
}

/// `FailedTransient` anchors whose exponential backoff window has elapsed go
/// back to `Queued` so the ready-builds pass can dispatch them again.
async fn transient_retries(ctx: &DbContext) -> anyhow::Result<u64> {
    let base = ctx.config.eval.build_retry_backoff_secs;
    let now = gradient_types::now();
    let transient = EDerivationBuild::find()
        .filter(CDerivationBuild::Status.eq(BuildStatus::FailedTransient))
        .all(&ctx.worker_db)
        .await?;
    let mut requeued = 0;
    for anchor in transient {
        if crate::policy::retry_backoff_elapsed(anchor.attempt, anchor.updated_at, now, base) {
            update_derivation_build_status(ctx, anchor, BuildStatus::Queued).await;
            requeued += 1;
        }
    }

    Ok(requeued)
}
