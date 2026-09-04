/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Dropping a path's claim on the cache index: one cache's claim, an operator
//! invalidation, a NAR the index lists but storage lost, or a maintenance sweep.

use anyhow::Result;
use gradient_db::{DbContext, collect_transitive_dependents};
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter};
use tracing::{info, warn};

use crate::messages::{DemoteReport, Demotion};

pub(crate) async fn apply(ctx: &DbContext, demotion: Demotion) -> Result<DemoteReport> {
    let db = &ctx.worker_db;
    let nar_storage = &ctx.storage.nar_storage;
    match demotion {
        Demotion::MissingNar { hash } => {
            let producers = gradient_db::demote_cached_output(db, nar_storage, &hash).await?;
            warn!(%hash, producers = producers.len(), "self-heal: NAR missing from storage; cached path demoted");
            Ok(DemoteReport {
                producers,
                ..Default::default()
            })
        }
        Demotion::Path { hash } => {
            let producers = gradient_db::demote_cached_output(db, nar_storage, &hash).await?;
            gradient_db::clear_gate_flags_for_hashes(db, std::slice::from_ref(&hash)).await?;
            gradient_db::clear_closure_complete_for_referrers(db, &hash).await?;
            for derivation in &producers {
                revoke_cache_derivation_closure(db, *derivation).await?;
            }

            info!(%hash, producers = producers.len(), "invalidated cache for path");
            Ok(DemoteReport {
                producers,
                ..Default::default()
            })
        }
        Demotion::CacheClaim { cache, hash } => cache_claim(ctx, cache, &hash).await,
        Demotion::UnbackedTrustedOutputs => {
            let demoted = gradient_db::demote_unbacked_trusted_outputs(db, nar_storage).await?;
            Ok(DemoteReport {
                demoted,
                ..Default::default()
            })
        }
    }
}

/// Remove a single cache's claim on a NAR: the per-cache signature row and the
/// per-cache derivation pins, plus - when no other cache still holds the path -
/// the shared `cached_path` row, the NAR blob and the gate flags they backed.
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

    let derivation_ids: Vec<DerivationId> = EDerivationOutput::find()
        .filter(CDerivationOutput::Hash.eq(hash))
        .all(db)
        .await?
        .into_iter()
        .map(|o| o.derivation)
        .collect();

    gradient_db::for_each_chunk(&derivation_ids, |chunk| async move {
        ECacheDerivation::delete_many()
            .filter(CCacheDerivation::Cache.eq(cache))
            .filter(CCacheDerivation::Derivation.is_in(chunk))
            .exec(db)
            .await
    })
    .await?;

    let remaining = ECachedPathSignature::find()
        .filter(CCachedPathSignature::CachedPath.eq(cached_path.id))
        .count(db)
        .await?;
    let others_remain = remaining > 0;

    // Last cache dropped the path: demote through the shared helper so the
    // producer anchor, gate flags and referrer closures reset symmetrically (a
    // bare is_cached clear leaves a Completed producer with no backing NAR).
    if !others_remain {
        let nar_storage = &ctx.storage.nar_storage;
        gradient_db::demote_cached_output(db, nar_storage, hash).await?;
        gradient_db::clear_gate_flags_for_hashes(db, &[hash.to_string()]).await?;
        gradient_db::clear_closure_complete_for_referrers(db, hash).await?;
    }

    let _ = ctx.board_events.send(BoardEvent::CacheChanged);

    Ok(DemoteReport {
        cached_path: Some(cached_path),
        others_remain,
        ..Default::default()
    })
}

/// Remove every `cache_derivation` row touching `derivation` and any of its
/// transitive dependents, across every cache.
async fn revoke_cache_derivation_closure<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<()> {
    let visited = collect_transitive_dependents(db, derivation).await?;
    let drv_ids: Vec<DerivationId> = visited.into_iter().collect();
    gradient_db::for_each_chunk(&drv_ids, |chunk| async move {
        ECacheDerivation::delete_many()
            .filter(CCacheDerivation::Derivation.is_in(chunk))
            .exec(db)
            .await
    })
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ctx::ctx;
    use sea_orm::{DatabaseBackend, MockDatabase};

    /// A cache dropping a claim on a path the index never held is not an error:
    /// the caller turns the absent row into its own 404.
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
