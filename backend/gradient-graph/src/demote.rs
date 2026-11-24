/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_db::DbContext;
use gradient_db::caches::demotion::{
    DemoteWhen, StoreContentToRetire, demote_cached_output,
    find_cached_path_for_store_hash_with_cache_signature, object_exists_in_storage_or_error,
};
use gradient_types::events::cache;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter};
use tracing::warn;

use crate::messages::{DemoteReport, Demotion};

fn delete_cached_path_signature_and_clear_references_are_claimed_sql() -> String {
    format!(
        "WITH deleted_cached_path_signatures AS ( \
             DELETE FROM cached_path_signature s USING cached_path cp \
             WHERE s.cached_path = cp.id AND s.cached_path = $1 AND s.cache = $2 \
             RETURNING s.id, s.cache, cp.hash, cp.package), \
         cleared AS ({clear}) \
         SELECT count(*) AS deleted FROM deleted_cached_path_signatures",
        clear = gradient_db::store_path_content_variant_trust::sql_text_clearing_references_are_claimed_for_cached_path_signature_rows_in_same_cache_referencing_deleted_signature_rows(
            "deleted_cached_path_signatures"
        ),
    )
}

gradient_db::sql_fn! {
    DELETE_CACHED_PATH_SIGNATURE_AND_CLEAR_REFERENCES_ARE_CLAIMED_SQL =
        delete_cached_path_signature_and_clear_references_are_claimed_sql,
        params = [CachedPathId, CacheId];
}

#[derive(Default)]
pub(crate) struct DemotionReportAndNarObjectKeysToDelete {
    report: DemoteReport,
    nar_object_keys_to_delete: Vec<String>,
}

pub(crate) async fn check_nar_object_exists_in_object_storage(
    ctx: &DbContext,
    demotion: &Demotion,
) -> bool {
    match demotion {
        Demotion::MissingNarOfContentVariant { content_variant } => {
            match ECachedPath::find_by_id(*content_variant)
                .one(&ctx.worker_db)
                .await
            {
                Ok(Some(cached_path)) => object_exists_in_storage_or_error(ctx, &cached_path).await,
                Ok(None) | Err(_) => true,
            }
        }
        Demotion::CacheClaim { cache, hash } => {
            match find_cached_path_for_store_hash_with_cache_signature(&ctx.worker_db, *cache, hash)
                .await
            {
                Ok(Some(cached_path)) => object_exists_in_storage_or_error(ctx, &cached_path).await,
                Ok(None) => false,
                Err(_) => true,
            }
        }
    }
}

pub(crate) async fn apply(
    ctx: &DbContext,
    demotion: Demotion,
    object_present: bool,
) -> Result<DemotionReportAndNarObjectKeysToDelete> {
    match demotion {
        Demotion::MissingNarOfContentVariant { content_variant } => {
            let producers = demote_cached_output(
                ctx,
                &StoreContentToRetire::OnlyTheseCachedPathIds(vec![content_variant]),
                DemoteWhen::ObjectMissing,
                object_present,
            )
            .await?
            .producers;
            warn!(%content_variant, producers = producers.len(), "self-heal: NAR of content variant missing from storage; content variant retired");
            Ok(DemotionReportAndNarObjectKeysToDelete {
                report: DemoteReport {
                    producers,
                    ..Default::default()
                },
                nar_object_keys_to_delete: Vec::new(),
            })
        }
        Demotion::CacheClaim { cache, hash } => cache_claim(ctx, cache, hash, object_present).await,
    }
}

pub(crate) async fn delete_nar_object_from_object_storage_after_demotion_commit(
    ctx: &DbContext,
    demoted: DemotionReportAndNarObjectKeysToDelete,
) -> DemoteReport {
    for key in &demoted.nar_object_keys_to_delete {
        if let Err(e) = ctx.storage.nar_storage.delete(key).await {
            warn!(%key, error = %e, "demote: failed to delete NAR object from storage");
        }
    }
    demoted.report
}

async fn cache_claim(
    ctx: &DbContext,
    cache: CacheId,
    hash: String,
    object_present: bool,
) -> Result<DemotionReportAndNarObjectKeysToDelete> {
    let db = &ctx.worker_db;
    let Some(cached_path) =
        find_cached_path_for_store_hash_with_cache_signature(db, cache, &hash).await?
    else {
        return Ok(DemotionReportAndNarObjectKeysToDelete::default());
    };

    db.execute_raw(
        DELETE_CACHED_PATH_SIGNATURE_AND_CLEAR_REFERENCES_ARE_CLAIMED_SQL
            .bind([cached_path.id.into(), cache.into()]),
    )
    .await?;

    let remaining = ECachedPathSignature::find()
        .filter(CCachedPathSignature::CachedPath.eq(cached_path.id))
        .count(db)
        .await?;
    let others_remain = remaining > 0;

    let nar_object_keys_to_delete = if others_remain {
        Vec::new()
    } else {
        demote_cached_output(
            ctx,
            &StoreContentToRetire::OnlyTheseCachedPathIds(vec![cached_path.id]),
            DemoteWhen::Always,
            object_present,
        )
        .await?
        .unreferenced_cached_path_object_storage_keys
    };

    ctx.events.publish(cache::Changed {});

    Ok(DemotionReportAndNarObjectKeysToDelete {
        report: DemoteReport {
            cached_path: Some(cached_path),
            others_remain,
            ..Default::default()
        },
        nar_object_keys_to_delete,
    })
}
