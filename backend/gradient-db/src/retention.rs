/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Background pruning of the metrics firehose tables so they stay bounded.
//!
//! Raw `phase_event` / `worker_sample` rows and `dispatched_job` forensic rows
//! are dropped past their configured age; `metric_rollup` minute/hour buckets
//! are pruned while day/week aggregates are kept indefinitely. All bounds come
//! from [`MetricsArgs`]; a `0` day-count disables that table's pruning.

use std::time::Duration;

use gradient_entity::metric_rollup::RollupGranularity;
use gradient_util::supervision::ChildSpec;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::{debug, warn};

use super::DbContext;

const RETENTION_INTERVAL_SECS: u64 = 3600;

/// The hourly pruning pass as a supervised child.
pub fn child_spec(ctx: DbContext) -> ChildSpec {
    ChildSpec::periodic(
        "retention",
        Duration::from_secs(RETENTION_INTERVAL_SECS),
        Duration::from_secs(600),
        move || {
            let ctx = ctx.clone();
            async move {
                run_retention(&ctx).await;
                Ok(())
            }
        },
    )
}

async fn run_retention(ctx: &DbContext) {
    let cfg = &ctx.config.metrics_args;
    let now = gradient_types::now();
    let db = &ctx.worker_db;

    if cfg.metrics_retention_raw_days > 0 {
        let cutoff = now - chrono::Duration::days(cfg.metrics_retention_raw_days);
        if let Err(e) = gradient_entity::phase_event::Entity::delete_many()
            .filter(gradient_entity::phase_event::Column::At.lt(cutoff))
            .exec(db)
            .await
        {
            warn!(error = %e, "phase_event retention failed");
        }
        if let Err(e) = gradient_entity::worker_sample::Entity::delete_many()
            .filter(gradient_entity::worker_sample::Column::At.lt(cutoff))
            .exec(db)
            .await
        {
            warn!(error = %e, "worker_sample retention failed");
        }
    }

    if cfg.dispatch_retention_days > 0 {
        let cutoff = now - chrono::Duration::days(cfg.dispatch_retention_days);
        if let Err(e) = gradient_entity::dispatched_job::Entity::delete_many()
            .filter(gradient_entity::dispatched_job::Column::CreatedAt.lt(cutoff))
            .exec(db)
            .await
        {
            warn!(error = %e, "dispatched_job retention failed");
        }
    }

    if cfg.metrics_retention_rollup_days > 0 {
        let cutoff = now - chrono::Duration::days(cfg.metrics_retention_rollup_days);
        if let Err(e) = gradient_entity::metric_rollup::Entity::delete_many()
            .filter(gradient_entity::metric_rollup::Column::BucketStart.lt(cutoff))
            .filter(
                gradient_entity::metric_rollup::Column::Granularity
                    .is_in([RollupGranularity::Minute, RollupGranularity::Hour]),
            )
            .exec(db)
            .await
        {
            warn!(error = %e, "metric_rollup retention failed");
        }
    }

    debug!("metrics retention pass complete");
}
