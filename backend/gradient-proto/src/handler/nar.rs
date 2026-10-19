/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::WorkerDb;
use gradient_db::metrics::cache_traffic::CacheTraffic;
use gradient_graph::{Graph, NarCommit, SignTargets};
use gradient_types::events::cache::NarSigned;
use gradient_types::ids::ProjectId;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

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
    cache_traffic.record(project_cache.cache, bucket, bytes);

    Ok(())
}

pub(super) async fn mark_nar_stored(
    graph: &Graph,
    events: &EventBus,
    project_id: ProjectId,
    store_path: &str,
    record: &NarUploadRecord<'_>,
) -> anyhow::Result<()> {
    let hash_name = store_path.strip_prefix("/nix/store/").unwrap_or(store_path);
    let hash = hash_name.split('-').next().unwrap_or("");

    if hash.is_empty() {
        return Ok(());
    }

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

    for cache in committed.signed {
        events.publish(NarSigned {
            cache,
            hash: hash.to_owned(),
        });
    }

    Ok(())
}
