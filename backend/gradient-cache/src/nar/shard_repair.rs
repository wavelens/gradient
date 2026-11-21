/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::expiry::MAX_CACHED_PATH_HASHES_PER_RETIRE_TRANSACTION;
use anyhow::{Context, Result};
use gradient_db::{DbContext, WorkerDb};
use gradient_graph::{GcRequest, Graph};
use gradient_storage::NarStore;
use gradient_types::events::gc::{Pass, Swept};
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Serialize;
use std::collections::HashSet;
use tracing::{debug, info, warn};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct CleanupReport {
    pub orphan_nars_scanned: u64,
    pub orphan_nars_removed: u64,
    pub zombie_cached_paths_purged: u64,
}

pub async fn repair_nar_shard(db: &DbContext, graph: &Graph, shard: &str) -> Result<CleanupReport> {
    let on_disk = db
        .storage
        .nar_storage
        .list_shard(shard)
        .await
        .context("list NAR store")?;
    let on_disk_set: HashSet<String> = on_disk.iter().map(|(h, _)| h.clone()).collect();

    let mut report = CleanupReport {
        orphan_nars_scanned: on_disk.len() as u64,
        ..Default::default()
    };
    let candidates = past_upload_grace(&db.config, &on_disk);
    for chunk in candidates.chunks(ORPHAN_CANDIDATE_BATCH) {
        report.orphan_nars_removed += delete_orphan_objects(graph, chunk).await?;
    }

    if report.orphan_nars_removed > 0 {
        info!(
            count = report.orphan_nars_removed,
            "Removed orphaned NAR files"
        );
        db.record(Swept {
            pass: Pass::OrphanNars,
            removed: report.orphan_nars_removed,
        })
        .await;
    }

    report.zombie_cached_paths_purged = purge_zombie_cached_paths(
        &db.worker_db,
        &db.storage.nar_storage,
        graph,
        shard,
        &on_disk_set,
    )
    .await?;
    Ok(report)
}

fn zombie_candidates(shard: &str) -> sea_orm::Select<ECachedPath> {
    let last = format!("{shard}{}", "z".repeat(32usize.saturating_sub(shard.len())));
    ECachedPath::find().filter(CCachedPath::Hash.between(shard.to_owned(), last))
}

/// The listing was taken before these rows were read. A NAR committed in between is absent from the
/// listing yet present in storage. Dropping its row would leave the producer `Completed` with no
/// backing output. Storage is the judge, and a probe error is preserving the row.
async fn load_shard_zombie_cached_path_row_ids(
    db: &WorkerDb,
    nar_storage: &NarStore,
    shard: &str,
    on_disk: &HashSet<String>,
) -> Result<Vec<CachedPathId>> {
    let rows = zombie_candidates(shard)
        .all(db)
        .await
        .context("load cached_path rows for zombie purge")?;

    let mut zombies = Vec::new();
    for row in rows {
        let key = &row.nar_object_storage_key;
        if on_disk.contains(key) {
            continue;
        }

        match nar_storage.exists(key).await {
            Ok(false) => zombies.push(row.id),
            Ok(true) => {
                debug!(%key, "zombie purge: the object landed after the listing")
            }
            Err(e) => {
                warn!(%key, error = %e, "zombie purge: could not probe the object")
            }
        }
    }

    Ok(zombies)
}

async fn purge_zombie_cached_paths(
    db: &WorkerDb,
    nar_storage: &NarStore,
    graph: &Graph,
    shard: &str,
    on_disk: &HashSet<String>,
) -> Result<u64> {
    let zombies = load_shard_zombie_cached_path_row_ids(db, nar_storage, shard, on_disk).await?;
    if zombies.is_empty() {
        return Ok(0);
    }

    let mut purged = 0u64;
    for chunk in zombies.chunks(MAX_CACHED_PATH_HASHES_PER_RETIRE_TRANSACTION) {
        match graph
            .gc(GcRequest::RetireCachedPathsWithMissingNarFiles {
                ids: chunk.to_vec(),
            })
            .await
        {
            Ok(report) => {
                purged += report.unreferenced_cached_path_object_storage_keys.len() as u64
            }
            Err(e) => {
                warn!(error = %e, batch = chunk.len(), "failed to purge zombie cached_path batch")
            }
        }
    }

    if purged > 0 {
        info!(
            count = purged,
            "Purged cached_path rows whose NAR is missing from storage"
        );
    }

    Ok(purged)
}

const ORPHAN_CANDIDATE_BATCH: usize = 5000;

/// A fresh NAR is on disk before the evaluation committed its `derivation` and `cached_path` rows.
/// Reclaiming it would strand a zombie `cached_path` that the dispatch gate is trusting. A grace of
/// `<= 0` is for tests only.
fn past_upload_grace(config: &RuntimeConfig, on_disk: &[(String, i64)]) -> Vec<String> {
    let cutoff = orphan_delete_cutoff_timestamp(config);
    on_disk
        .iter()
        .filter(|(_, modified)| *modified < cutoff)
        .map(|(hash, _)| hash.clone())
        .collect()
}

pub(crate) fn orphan_delete_cutoff_timestamp(config: &RuntimeConfig) -> i64 {
    let grace_secs = i64::from(config.gc.nar_upload_grace_hours) * 3600;
    if grace_secs > 0 {
        now().and_utc().timestamp() - grace_secs
    } else {
        i64::MAX
    }
}

async fn delete_orphan_objects(graph: &Graph, candidates: &[String]) -> Result<u64> {
    let report = graph
        .gc(GcRequest::OrphanObjects {
            object_storage_keys: candidates.to_vec(),
        })
        .await
        .context("delete orphaned NAR objects")?;
    Ok(report.unreferenced_orphan_object_storage_keys.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_server_state;
    use gradient_core::ServerState;
    use gradient_fixtures::mock::mock_db;
    use gradient_storage::NarStore;
    use std::path::Path;
    use std::sync::Arc;

    fn write_nar_file(base: &Path, hash: &str) {
        let dir = base.join("nars").join(&hash[..2]);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{}.nar.zst", &hash[2..])), b"x").unwrap();
    }

    fn nar_file_exists(base: &Path, hash: &str) -> bool {
        base.join("nars")
            .join(&hash[..2])
            .join(format!("{}.nar.zst", &hash[2..]))
            .exists()
    }

    fn make_state(base: &Path) -> Arc<ServerState> {
        let nar_storage = NarStore::local(base).unwrap();
        let db = mock_db()
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();
        test_server_state(nar_storage, db, |config| {
            config.gc.nar_upload_grace_hours = 0;
        })
    }

    #[tokio::test]
    async fn every_unreferenced_candidate_is_removed_and_a_referenced_one_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let active_hash = "aabbccdd111111111111111111111111";
        let active = &format!("{active_hash}-abc");
        let h1 = "aaff0011222222222222222222222222";
        let h2 = "aa22bbbb555555555555555555555555";
        for key in [active, h1, h2] {
            write_nar_file(tmp.path(), key);
        }
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path(active_hash, "pkg")
            .into_database()
            .await;
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(tmp.path()).unwrap(),
            db.connection(),
            |config| config.gc.nar_upload_grace_hours = 0,
        )
        .await;

        let report = repair_nar_shard(&state.db(), &state.graph, "aa")
            .await
            .unwrap();

        assert!(nar_file_exists(tmp.path(), active));
        assert!(!nar_file_exists(tmp.path(), h1));
        assert!(!nar_file_exists(tmp.path(), h2));
        assert_eq!(report.orphan_nars_scanned, 3);
        assert_eq!(report.orphan_nars_removed, 2);
        db.finish().await;
    }

    #[test]
    fn only_nars_past_the_upload_grace_are_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path());
        Arc::make_mut(&mut Arc::get_mut(&mut state).unwrap().config)
            .gc
            .nar_upload_grace_hours = 24;
        let fresh = now().and_utc().timestamp();
        let listed = vec![
            ("old".to_owned(), fresh - 25 * 3600),
            ("fresh".to_owned(), fresh),
        ];

        assert_eq!(
            past_upload_grace(&state.config, &listed),
            vec!["old".to_owned()]
        );
    }

    #[tokio::test]
    async fn fresh_orphan_nar_spared_within_grace() {
        let tmp = tempfile::tempdir().unwrap();
        let orphan = "ddccbbaa99999999999999999999999999";
        write_nar_file(tmp.path(), orphan);

        let nar_storage = NarStore::local(tmp.path()).unwrap();
        let only_the_zombie_load = mock_db()
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();
        let state = test_server_state(nar_storage, only_the_zombie_load, |config| {
            config.gc.nar_upload_grace_hours = 24;
        });

        repair_nar_shard(&state.db(), &state.graph, "dd")
            .await
            .expect("a pass with no candidate issues no probe");
        assert!(
            nar_file_exists(tmp.path(), orphan),
            "freshly written orphan NAR must be spared within the grace window"
        );
    }

    #[tokio::test]
    async fn a_path_committed_after_the_listing_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = "cccc33333333333333333333333333cccc";
        write_nar_file(tmp.path(), fresh);

        let row = gradient_entity::cached_path::Model {
            id: CachedPathId::now_v7(),
            hash: fresh.into(),
            nar_object_storage_key: fresh.into(),
            package: "fresh".into(),
            file_hash: "sha256:beef".into(),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![row]])
            .into_connection();

        let nar_storage = NarStore::local(tmp.path()).unwrap();
        let state = test_server_state(nar_storage, db, |_| {});

        let zombies = load_shard_zombie_cached_path_row_ids(
            &state.worker_db,
            &state.nar_storage,
            "cc",
            &HashSet::new(),
        )
        .await
        .unwrap();

        assert!(zombies.is_empty(), "storage decides, not the stale listing");
        assert!(nar_file_exists(tmp.path(), fresh));
    }

    #[tokio::test]
    async fn names_cached_paths_whose_nar_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let live = "aaaa11111111111111111111111111aaaa";
        let zombie_hash = "bbbb22222222222222222222222222bbbb";

        write_nar_file(tmp.path(), live);

        let nar_storage = NarStore::local(tmp.path()).unwrap();
        let zombie_id = CachedPathId::now_v7();
        let rows = vec![
            gradient_entity::cached_path::Model {
                id: CachedPathId::now_v7(),
                hash: live.into(),
                nar_object_storage_key: live.into(),
                package: "live".into(),
                file_hash: "sha256:live".into(),
                ..Default::default()
            },
            gradient_entity::cached_path::Model {
                id: zombie_id,
                hash: zombie_hash.into(),
                nar_object_storage_key: zombie_hash.into(),
                package: "zombie".into(),
                file_hash: "sha256:zombie".into(),
                ..Default::default()
            },
        ];
        let db = mock_db().append_query_results([rows]).into_connection();

        let state = test_server_state(nar_storage, db, |_| {});

        let zombies = load_shard_zombie_cached_path_row_ids(
            &state.worker_db,
            &state.nar_storage,
            "",
            &HashSet::new(),
        )
        .await
        .unwrap();

        assert_eq!(zombies, vec![zombie_id]);
        assert!(nar_file_exists(tmp.path(), live), "live NAR must survive");
    }

    async fn object_keys_of_cached_paths(
        db: &gradient_fixtures::database::TestDatabase,
    ) -> Vec<String> {
        use sea_orm::QueryOrder;
        ECachedPath::find()
            .order_by_asc(CCachedPath::NarObjectStorageKey)
            .all(&db.connection())
            .await
            .expect("the cached paths are read")
            .into_iter()
            .map(|row| row.nar_object_storage_key)
            .collect()
    }

    #[tokio::test]
    async fn an_object_under_a_per_content_key_survives_the_orphan_pass_while_its_row_refers_to_it()
    {
        let hash = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
        let key = format!("{hash}-c0ntentfi1ehash");
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path(hash, "pkg")
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE cached_path SET nar_object_storage_key = '{key}' WHERE hash = '{hash}'"
        ))
        .await;
        let tmp = tempfile::tempdir().unwrap();
        write_nar_file(tmp.path(), &key);
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(tmp.path()).unwrap(),
            db.connection(),
            |config| config.gc.nar_upload_grace_hours = 0,
        )
        .await;

        repair_nar_shard(&state.db(), &state.graph, "cd")
            .await
            .unwrap();

        assert!(
            nar_file_exists(tmp.path(), &key),
            "Objects stored under a per-content key remain for as long as cached path rows reference them."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn an_orphan_that_an_upload_commits_on_after_the_listing_keeps_its_object() {
        let hash = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";
        let tmp = tempfile::tempdir().unwrap();
        write_nar_file(tmp.path(), hash);
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .into_database()
            .await;
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(tmp.path()).unwrap(),
            db.connection(),
            |config| config.gc.nar_upload_grace_hours = 0,
        )
        .await;
        let listed = past_upload_grace(&state.config, &[(hash.to_owned(), 0)]);
        assert_eq!(listed, vec![hash.to_owned()]);

        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, \
                 file_size, nar_size, nar_hash, debug_info_indexed, \
                 server_computed_hashes_and_sizes, created_at) \
             VALUES (uuidv7(), '{hash}', '{hash}', 'pkg', 'sha256:file', 1, 1, \
                 'sha256:nar', false, false, now())"
        ))
        .await;
        delete_orphan_objects(&state.graph, &listed).await.unwrap();

        assert_eq!(
            object_keys_of_cached_paths(&db).await,
            vec![hash.to_owned()]
        );
        assert!(
            nar_file_exists(tmp.path(), hash),
            "An upload that commits on a listed orphan keeps the object its row serves."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn an_upload_that_skipped_its_write_is_refused_when_it_commits_after_the_orphan_delete() {
        use gradient_entity::store_path_content_producer::CacheUploadChannel;
        use gradient_graph::{NarCommit, NarCommitContent, StoredContentProducer};

        let hash = "c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2";
        let cache = CacheId::now_v7();
        let facts = gradient_storage::compress_nar_bytes_and_verify_stream(b"nar");
        let key = facts.object_storage_key_for_store_path_hash_and_compressed_file_sha256(hash);
        let tmp = tempfile::tempdir().unwrap();
        write_nar_file(tmp.path(), &key);
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .into_cache(cache)
            .into_database()
            .await;
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(tmp.path()).unwrap(),
            db.connection(),
            |config| config.gc.nar_upload_grace_hours = 0,
        )
        .await;
        let listed = past_upload_grace(&state.config, &[(key.clone(), 0)]);

        delete_orphan_objects(&state.graph, &listed).await.unwrap();
        let skipped_write = state
            .graph
            .commit_nar(NarCommit {
                store_path: format!("/nix/store/{hash}-pkg"),
                content: NarCommitContent::ServerComputedNarAndCompressedFileHashesAndSizes(facts),
                references: Vec::new(),
                deriver: None,
                ca: None,
                producer: StoredContentProducer::Upload {
                    caches: vec![cache],
                    user: None,
                    api_key: None,
                    channel: CacheUploadChannel::RestHttpApiUpload,
                },
            })
            .await;

        assert!(
            !nar_file_exists(tmp.path(), &key),
            "The orphan delete removes the listed object."
        );
        assert!(
            skipped_write.is_err() && object_keys_of_cached_paths(&db).await.is_empty(),
            "An upload queued behind the orphan delete is refused, so no row names the deleted object: {skipped_write:?}"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn the_zombie_purge_retires_only_the_content_whose_object_is_missing() {
        let hash = "cececececececececececececececece";
        let present = format!("{hash}-presentc0ntent");
        let missing = format!("{hash}-missingc0ntent");
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path(hash, "pkg")
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE cached_path SET nar_object_storage_key = '{present}' WHERE hash = '{hash}'; \
             INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, \
                 file_size, nar_size, nar_hash, debug_info_indexed, \
                 server_computed_hashes_and_sizes, created_at) \
             SELECT uuidv7(), hash, '{missing}', package, 'sha256:other', file_size, nar_size, \
                 'sha256:othernar', false, false, created_at \
             FROM cached_path WHERE hash = '{hash}';"
        ))
        .await;
        let tmp = tempfile::tempdir().unwrap();
        write_nar_file(tmp.path(), &present);
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(tmp.path()).unwrap(),
            db.connection(),
            |config| config.gc.nar_upload_grace_hours = 24,
        )
        .await;

        repair_nar_shard(&state.db(), &state.graph, "ce")
            .await
            .unwrap();

        assert_eq!(
            object_keys_of_cached_paths(&db).await,
            vec![present.clone()],
            "Only the content whose object is missing can lose its row."
        );
        assert!(nar_file_exists(tmp.path(), &present));
        db.finish().await;
    }
}
