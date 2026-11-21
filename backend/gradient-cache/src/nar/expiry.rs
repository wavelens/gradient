/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_db::DbContext;
use gradient_graph::{GcRequest, Graph};
use gradient_types::events::gc::{Pass, Swept};
use gradient_types::*;
use tracing::info;

pub(crate) const MAX_CACHED_PATH_HASHES_PER_RETIRE_TRANSACTION: usize = 1_000;

fn keep_hours(ttl_hours: u64, grace_hours: i64) -> i64 {
    (ttl_hours as i64).max(grace_hours)
}

pub(crate) async fn evict_expired_cached_paths(db: &DbContext, graph: &Graph) -> Result<u64> {
    evict_expired_cached_paths_with_max_hashes_per_retire_transaction(
        db,
        graph,
        MAX_CACHED_PATH_HASHES_PER_RETIRE_TRANSACTION,
    )
    .await
}

async fn evict_expired_cached_paths_with_max_hashes_per_retire_transaction(
    db: &DbContext,
    graph: &Graph,
    max_cached_path_hashes_per_retire_transaction: usize,
) -> Result<u64> {
    let scanned_at = now();
    let keep = keep_hours(
        db.config.gc.nar_ttl_hours,
        i64::from(db.config.gc.nar_upload_grace_hours),
    );
    let unused_since = scanned_at - chrono::Duration::hours(keep);
    let expired =
        gradient_db::maintenance::gc::scan_deletable_path_hashes(&db.worker_db, unused_since)
            .await
            .context("select expired cached paths")?;

    let (mut evicted, mut deleted_claim_row_count) = (0u64, 0u64);
    for hashes in expired.chunks(max_cached_path_hashes_per_retire_transaction) {
        let report = graph
            .gc(GcRequest::UnusedCacheClaims {
                hashes: hashes.to_vec(),
                unused_since,
                scanned_at,
            })
            .await
            .context("retire expired paths")?;
        evicted += report.unreferenced_cached_path_object_storage_keys.len() as u64;
        deleted_claim_row_count += report.deleted_claim_row_count;
    }
    if deleted_claim_row_count == 0 && evicted == 0 {
        return Ok(0);
    }

    info!(
        evicted,
        deleted_claim_row_count, "Evicted expired cached paths"
    );
    db.events.publish(gradient_types::events::cache::Changed {});
    if evicted > 0 {
        db.record(Swept {
            pass: Pass::StaleCachedPaths,
            removed: evicted,
        })
        .await;
    }
    Ok(evicted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eviction_bound_never_undercuts_the_upload_grace() {
        assert_eq!(keep_hours(0, 24), 24);
        assert_eq!(keep_hours(336, 24), 336);
    }

    #[tokio::test]
    async fn expiry_retires_only_the_content_whose_claim_went_unused() {
        use gradient_storage::NarStore;
        use sea_orm::EntityTrait;

        let hash = "dfdfdfdfdfdfdfdfdfdfdfdfdfdfdfdf";
        let kept = format!("{hash}-keptc0ntent");
        let (unused_cache, used_cache) = (CacheId::now_v7(), CacheId::now_v7());
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .into_cache(unused_cache)
            .into_cache(used_cache)
            .cached_path(hash, "pkg")
            .into_database()
            .await;
        let retired_cached_path_object_storage_key = db
            .cached_path(hash)
            .await
            .expect("The cached path for the fixture is present.")
            .nar_object_storage_key;
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, \
                 file_size, nar_size, nar_hash, debug_info_indexed, \
                 server_computed_hashes_and_sizes, created_at) \
             SELECT uuidv7(), hash, '{kept}', package, 'sha256:other', file_size, nar_size, \
                 'sha256:othernar', false, false, created_at \
             FROM cached_path WHERE hash = '{hash}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             SELECT uuidv7(), id, '{unused_cache}', 0, now() - interval '10 days' \
             FROM cached_path WHERE nar_object_storage_key = '{retired_cached_path_object_storage_key}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             SELECT uuidv7(), id, '{used_cache}', 0, now() \
             FROM cached_path WHERE nar_object_storage_key = '{kept}';"
        ))
        .await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let nar = NarStore::local(tmp.path()).expect("store");
        for key in [
            retired_cached_path_object_storage_key.as_str(),
            kept.as_str(),
        ] {
            nar.put(key, b"nar".to_vec()).await.expect("put");
        }
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(tmp.path()).expect("store"),
            db.connection(),
            |config| {
                config.gc.nar_ttl_hours = 1;
                config.gc.nar_upload_grace_hours = 0;
            },
        )
        .await;

        evict_expired_cached_paths(&state.db(), &state.graph)
            .await
            .expect("the expiry runs");

        let keys: Vec<String> = ECachedPath::find()
            .all(&db.connection())
            .await
            .expect("the cached paths are read")
            .into_iter()
            .map(|row| row.nar_object_storage_key)
            .collect();
        assert_eq!(
            keys,
            vec![kept.clone()],
            "Contents with unused claims retire. Contents with new claims stay."
        );
        assert_eq!(
            (
                nar.exists(&retired_cached_path_object_storage_key)
                    .await
                    .expect("probe"),
                nar.exists(&kept).await.expect("probe")
            ),
            (false, true),
            "Runs of the expiry pass delete the objects of retired content. The objects of remaining content stay."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn eviction_of_five_expired_paths_with_two_hashes_per_retire_sends_three_retire_requests_to_graph_writer()
     {
        let (hash, cache) = ("dfdfdfdfdfdfdfdfdfdfdfdfdfdfdfdf", CacheId::now_v7());
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .into_cache(cache)
            .cached_path(hash, "pkg")
            .into_database()
            .await;
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, \
                 file_size, nar_size, nar_hash, debug_info_indexed, \
                 server_computed_hashes_and_sizes, created_at) \
             SELECT uuidv7(), lpad(i::text, 32, '0'), lpad(i::text, 32, '0') || '-expired', \
                 package, 'sha256:' || i, file_size, nar_size, 'sha256:nar' || i, false, false, \
                 created_at \
             FROM cached_path, generate_series(1, 4) AS i WHERE hash = '{hash}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             SELECT uuidv7(), id, '{cache}', 0, now() - interval '10 days' FROM cached_path;"
        ))
        .await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            gradient_storage::NarStore::local(tmp.path()).expect("store"),
            db.connection(),
            |config| {
                config.gc.nar_ttl_hours = 1;
                config.gc.nar_upload_grace_hours = 0;
            },
        )
        .await;
        let mut events = state.db().events.subscribe();

        evict_expired_cached_paths_with_max_hashes_per_retire_transaction(
            &state.db(),
            &state.graph,
            2,
        )
        .await
        .expect("the expiry runs");

        let retire_requests = std::iter::from_fn(|| events.try_recv().ok())
            .filter(|envelope| {
                envelope.event.name() == gradient_types::events::names::GRAPH_COLLECTED
            })
            .count();
        assert_eq!(retire_requests, 3);
        db.finish().await;
    }

    #[tokio::test]
    async fn retired_keys_leave_storage_and_refused_keys_stay() {
        use gradient_storage::NarStore;
        use sea_orm::EntityTrait;

        let hash = "dfdfdfdfdfdfdfdfdfdfdfdfdfdfdfdf";
        let generated = 2 * MAX_CACHED_PATH_HASHES_PER_RETIRE_TRANSACTION;
        let (unused_cache, used_cache) = (CacheId::now_v7(), CacheId::now_v7());
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .into_cache(unused_cache)
            .into_cache(used_cache)
            .cached_path(hash, "pkg")
            .into_database()
            .await;
        let mut expired: Vec<String> = (1..=generated)
            .map(|i| format!("{i:0>32}-expired"))
            .collect();
        let refused = format!("{}-refused", &expired[0][..32]);
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, \
                 file_size, nar_size, nar_hash, debug_info_indexed, \
                 server_computed_hashes_and_sizes, created_at) \
             SELECT uuidv7(), lpad(i::text, 32, '0'), lpad(i::text, 32, '0') || '-expired', \
                 package, 'sha256:' || i, file_size, nar_size, 'sha256:nar' || i, false, false, \
                 created_at \
             FROM cached_path, generate_series(1, {generated}) AS i WHERE hash = '{hash}'; \
             INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, \
                 file_size, nar_size, nar_hash, debug_info_indexed, \
                 server_computed_hashes_and_sizes, created_at) \
             SELECT uuidv7(), hash, '{refused}', package, 'sha256:other', file_size, nar_size, \
                 'sha256:othernar', false, false, created_at \
             FROM cached_path WHERE nar_object_storage_key = '{first}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             SELECT uuidv7(), id, '{unused_cache}', 0, now() - interval '10 days' \
             FROM cached_path WHERE nar_object_storage_key <> '{refused}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             SELECT uuidv7(), id, '{used_cache}', 0, now() \
             FROM cached_path WHERE nar_object_storage_key = '{refused}';",
            first = expired[0],
        ))
        .await;
        expired.push(
            db.cached_path(hash)
                .await
                .expect("The cached path for the fixture is present.")
                .nar_object_storage_key,
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let nar = NarStore::local(tmp.path()).expect("store");
        for key in expired.iter().chain([&refused]) {
            nar.put(key, b"nar".to_vec()).await.expect("put");
        }
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(tmp.path()).expect("store"),
            db.connection(),
            |config| {
                config.gc.nar_ttl_hours = 1;
                config.gc.nar_upload_grace_hours = 0;
            },
        )
        .await;

        let evicted = evict_expired_cached_paths(&state.db(), &state.graph)
            .await
            .expect("the expiry runs");

        assert_eq!(usize::try_from(evicted).ok(), Some(expired.len()));
        let keys: Vec<String> = ECachedPath::find()
            .all(&db.connection())
            .await
            .expect("the cached paths are read")
            .into_iter()
            .map(|row| row.nar_object_storage_key)
            .collect();
        assert_eq!(keys, vec![refused.clone()]);
        for key in expired.iter().chain([&refused]) {
            let stays = *key == refused;
            assert_eq!(nar.exists(key).await.expect("exists"), stays, "{key}");
        }
        db.finish().await;
    }
}
