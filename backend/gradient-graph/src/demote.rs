/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_db::DbContext;
use gradient_db::caches::demotion::{DemoteWhen, demote_cached_output};
use gradient_types::events::cache;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use tracing::{info, warn};

use crate::messages::{DemoteReport, Demotion};

pub(crate) async fn apply(ctx: &DbContext, demotion: Demotion) -> Result<DemoteReport> {
    match demotion {
        Demotion::MissingNar { hash } => {
            let producers = demote_cached_output(ctx, &hash, DemoteWhen::ObjectMissing).await?;
            warn!(%hash, producers = producers.len(), "self-heal: NAR missing from storage; cached path demoted");
            Ok(DemoteReport {
                producers,
                ..Default::default()
            })
        }
        Demotion::Path { hash } => {
            let producers = demote_cached_output(ctx, &hash, DemoteWhen::Always).await?;
            info!(%hash, producers = producers.len(), "invalidated cache for path");
            Ok(DemoteReport {
                producers,
                ..Default::default()
            })
        }
        Demotion::CacheClaim { cache, hash } => cache_claim(ctx, cache, &hash).await,
    }
}

async fn cache_claim(ctx: &DbContext, cache: CacheId, hash: &str) -> Result<DemoteReport> {
    let db = &ctx.worker_db;
    let Some(cached_path) = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .one(db)
        .await?
    else {
        return Ok(DemoteReport::default());
    };

    let Some(sig) = ECachedPathSignature::find()
        .filter(CCachedPathSignature::CachedPath.eq(cached_path.id))
        .filter(CCachedPathSignature::Cache.eq(cache))
        .one(db)
        .await?
    else {
        return Ok(DemoteReport::default());
    };

    ECachedPathSignature::delete_by_id(sig.id).exec(db).await?;

    let remaining = ECachedPathSignature::find()
        .filter(CCachedPathSignature::CachedPath.eq(cached_path.id))
        .count(db)
        .await?;
    let others_remain = remaining > 0;

    // The shared helper is resetting the producer, gate flags and parent counters together.
    // A bare `is_cached` clear would leave a `Completed` producer with no NAR behind it.
    if !others_remain {
        demote_cached_output(ctx, hash, DemoteWhen::Always).await?;
    }

    ctx.events.publish(cache::Changed {});

    Ok(DemoteReport {
        cached_path: Some(cached_path),
        others_remain,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ctx::ctx;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn an_unknown_path_reports_no_cached_path() {
        let (ctx, _) = ctx(MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .into_connection())
        .await;

        let report = apply(
            &ctx,
            Demotion::CacheClaim {
                cache: CacheId::now_v7(),
                hash: "a".into(),
            },
        )
        .await
        .expect("an unknown path is not an error");

        assert!(report.cached_path.is_none());
        assert!(!report.others_remain);
    }
}
