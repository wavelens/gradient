/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_db::DbContext;
use gradient_db::caches::demotion::{
    DemoteWhen, demote_cached_output, object_exists_in_storage_or_error,
};
use gradient_types::events::cache;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use tracing::{info, warn};

use crate::messages::{DemoteReport, Demotion};

#[derive(Default)]
pub(crate) struct DemoteReportAndObjectHashToDelete {
    report: DemoteReport,
    nar_object_hash_to_delete: Option<String>,
}

pub(crate) async fn check_nar_object_exists_in_object_storage(
    ctx: &DbContext,
    demotion: &Demotion,
) -> bool {
    let (Demotion::MissingNar { hash }
    | Demotion::Path { hash }
    | Demotion::CacheClaim { hash, .. }) = demotion;
    object_exists_in_storage_or_error(ctx, hash).await
}

pub(crate) async fn apply(
    ctx: &DbContext,
    demotion: Demotion,
    object_present: bool,
) -> Result<DemoteReportAndObjectHashToDelete> {
    match demotion {
        Demotion::MissingNar { hash } => {
            let producers =
                demote_cached_output(ctx, &hash, DemoteWhen::ObjectMissing, object_present).await?;
            warn!(%hash, producers = producers.len(), "self-heal: NAR missing from storage; cached path demoted");
            Ok(DemoteReportAndObjectHashToDelete {
                report: DemoteReport {
                    producers,
                    ..Default::default()
                },
                nar_object_hash_to_delete: None,
            })
        }
        Demotion::Path { hash } => {
            let producers =
                demote_cached_output(ctx, &hash, DemoteWhen::Always, object_present).await?;
            info!(%hash, producers = producers.len(), "invalidated cache for path");
            Ok(DemoteReportAndObjectHashToDelete {
                nar_object_hash_to_delete: (!producers.is_empty()).then_some(hash),
                report: DemoteReport {
                    producers,
                    ..Default::default()
                },
            })
        }
        Demotion::CacheClaim { cache, hash } => cache_claim(ctx, cache, hash, object_present).await,
    }
}

pub(crate) async fn delete_nar_object_from_object_storage_after_demotion_commit(
    ctx: &DbContext,
    demoted: DemoteReportAndObjectHashToDelete,
) -> DemoteReport {
    if let Some(hash) = &demoted.nar_object_hash_to_delete
        && let Err(e) = ctx.storage.nar_storage.delete(hash).await
    {
        warn!(%hash, error = %e, "demote: failed to delete NAR object from storage");
    }
    demoted.report
}

async fn cache_claim(
    ctx: &DbContext,
    cache: CacheId,
    hash: String,
    object_present: bool,
) -> Result<DemoteReportAndObjectHashToDelete> {
    let db = &ctx.worker_db;
    let Some(cached_path) = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(&hash))
        .one(db)
        .await?
    else {
        return Ok(DemoteReportAndObjectHashToDelete::default());
    };

    let Some(sig) = ECachedPathSignature::find()
        .filter(CCachedPathSignature::CachedPath.eq(cached_path.id))
        .filter(CCachedPathSignature::Cache.eq(cache))
        .one(db)
        .await?
    else {
        return Ok(DemoteReportAndObjectHashToDelete::default());
    };

    ECachedPathSignature::delete_by_id(sig.id).exec(db).await?;

    let remaining = ECachedPathSignature::find()
        .filter(CCachedPathSignature::CachedPath.eq(cached_path.id))
        .count(db)
        .await?;
    let others_remain = remaining > 0;

    let demoted_producers = if others_remain {
        Vec::new()
    } else {
        demote_cached_output(ctx, &hash, DemoteWhen::Always, object_present).await?
    };

    ctx.events.publish(cache::Changed {});

    Ok(DemoteReportAndObjectHashToDelete {
        report: DemoteReport {
            cached_path: Some(cached_path),
            others_remain,
            ..Default::default()
        },
        nar_object_hash_to_delete: (!demoted_producers.is_empty()).then_some(hash),
    })
}
