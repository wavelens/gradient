/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::{
    DbContext,
    graph::can_start::promote,
    status::{emit_transition_effects, update_derivation_build_status},
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

/// A retry committed as `Queued` is dispatchable before anything checks its inputs.
async fn transient_retries(ctx: &DbContext) -> anyhow::Result<u64> {
    let base = ctx.config.build.retry_backoff_secs;
    let now = gradient_types::now();
    let transient = EDerivationBuild::find()
        .filter(CDerivationBuild::Status.eq(BuildStatus::FailedTransient))
        .all(&ctx.worker_db)
        .await?;
    let mut requeued = Vec::new();
    for shared_build in transient {
        if crate::policy::retry_backoff_elapsed(
            shared_build.attempt,
            shared_build.updated_at,
            now,
            base,
        ) {
            let derivation = shared_build.derivation;
            update_derivation_build_status(ctx, shared_build, BuildStatus::Created).await?;
            requeued.push(derivation);
        }
    }

    let queued = promote(&ctx.worker_db, &requeued).await?;
    if queued.len() < requeued.len() {
        debug!(
            queued = queued.len(),
            requeued = requeued.len(),
            "retried shared builds wait in Created until their inputs can be fetched"
        );
    }

    emit_transition_effects(ctx, &queued).await?;

    Ok(requeued.len() as u64)
}
