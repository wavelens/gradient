/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::metrics::cache_traffic::{CacheTraffic, TrafficSource};
use gradient_db::{DbContext, WorkerDb};
use gradient_graph::{Graph, NarCommit, SignTargets};
use gradient_types::events::cache::NarSigned;
use gradient_types::ids::ProjectId;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;

pub(super) struct NarUploadRecord<'a> {
    pub file_hash: &'a str,
    pub file_size: i64,
    pub nar_size: i64,
    pub nar_hash: &'a str,
    pub references: &'a [String],
    pub deriver: Option<&'a str>,
    pub ca: Option<&'a str>,
    pub confirmed: bool,
}

pub(super) async fn record_nar_push_metric(
    db: &WorkerDb,
    cache_traffic: &CacheTraffic,
    project_id: ProjectId,
    bytes: i64,
) -> anyhow::Result<()> {
    let project_cache = EProjectCache::find()
        .filter(CProjectCache::Project.eq(project_id))
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no cache for project {}", project_id))?;

    let bucket = gradient_db::metrics::cache_traffic::minute_bucket(gradient_types::now());
    cache_traffic.record(project_cache.cache, bucket, bytes, TrafficSource::Internal);

    Ok(())
}

pub(super) async fn mark_nar_stored(
    graph: &Graph,
    db: &DbContext,
    project_id: ProjectId,
    store_path: &str,
    record: &NarUploadRecord<'_>,
) -> anyhow::Result<()> {
    let Ok(path) = StorePath::parse_stored(store_path) else {
        return Ok(());
    };

    let committed = graph
        .commit_nar(NarCommit {
            store_path: store_path.to_owned(),
            file_hash: record.file_hash.to_owned(),
            file_size: record.file_size,
            nar_size: record.nar_size,
            nar_hash: record.nar_hash.to_owned(),
            references: record.references.to_vec(),
            deriver: record.deriver.map(str::to_owned),
            ca: record.ca.map(str::to_owned),
            targets: SignTargets::ProjectCaches(project_id),
            confirmed: record.confirmed,
            built_by_worker: true,
        })
        .await?;

    for event in signed_events(&db.store_path_mapping, &committed.signed, &path) {
        db.events.publish(event);
    }

    Ok(())
}

fn signed_events(
    mapping: &Arc<dyn StorePathMapping>,
    caches: &[CacheId],
    path: &StorePath,
) -> Vec<NarSigned> {
    caches
        .iter()
        .map(|&cache| NarSigned {
            cache,
            hash: StorePathMapper::for_cache(Arc::clone(mapping), cache)
                .outbound(path)
                .hash()
                .to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};

    #[test]
    fn a_signed_nar_is_announced_with_its_original_hash() {
        let mapping: Arc<dyn StorePathMapping> = Arc::new(RotatedHashMapping);
        let path = StorePath::parse_stored(&format!("/nix/store/{EXAMPLE_STORED_HASH}-hello"))
            .expect("a stored path");
        let caches = vec![CacheId::now_v7(), CacheId::now_v7()];

        let events = signed_events(&mapping, &caches, &path);

        assert_eq!(events.len(), 2);
        for (event, cache) in events.iter().zip(&caches) {
            assert_eq!(event.cache, *cache);
            assert_eq!(event.hash, EXAMPLE_ORIGINAL_HASH);
        }
    }
}
