/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_graph::GcRequest;
use gradient_types::events::gc::{Pass, Swept};
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct CleanupReport {
    pub orphan_nars_scanned: u64,
    pub orphan_nars_removed: u64,
    pub zombie_cached_paths_purged: u64,
}

gradient_db::sql! {
    UNREFERENCED_HASHES = r#"
    SELECT h.hash AS hash
    FROM unnest($1::text[]) AS h(hash)
    WHERE NOT EXISTS (
        SELECT 1 FROM cached_path cp
        WHERE cp.hash = h.hash AND cp.file_hash IS NOT NULL)
    AND NOT EXISTS (
        SELECT 1 FROM derivation_output dout
        JOIN derivation_build b ON b.derivation = dout.derivation
        WHERE dout.hash = h.hash AND b.status NOT IN ($2, $3, $4, $5))
    AND NOT EXISTS (
        SELECT 1 FROM derivation_input_source s
        JOIN derivation_build b ON b.derivation = s.derivation
        WHERE s.hash = h.hash)
    AND NOT EXISTS (
        SELECT 1 FROM derivation d
        JOIN derivation_build b ON b.derivation = d.id
        WHERE d.hash = h.hash)
"#,
        params = [CachedPathHashes(UNREFERENCED_PROBE_BATCH), Int(4), Int(5), Int(6), Int(9)],
        tier = Sweep;
}

pub async fn repair_nar_shard(state: Arc<ServerState>, shard: &str) -> Result<CleanupReport> {
    let on_disk = state
        .nar_storage
        .list_shard(shard)
        .await
        .context("Failed to list NAR store")?;
    let on_disk_set: HashSet<String> = on_disk.iter().map(|(h, _)| h.clone()).collect();

    let mut report = CleanupReport {
        orphan_nars_scanned: on_disk.len() as u64,
        ..Default::default()
    };
    let candidates = past_upload_grace(&state, &on_disk);
    for chunk in candidates.chunks(UNREFERENCED_PROBE_BATCH) {
        for hash in unreferenced_hashes(&state, chunk).await? {
            if remove_orphan_nar(&state, &hash).await {
                report.orphan_nars_removed += 1;
            }
        }
    }

    if report.orphan_nars_removed > 0 {
        info!(
            count = report.orphan_nars_removed,
            "Removed orphaned NAR files"
        );
        state
            .record(Swept {
                pass: Pass::OrphanNars,
                removed: report.orphan_nars_removed,
            })
            .await;
    }

    report.zombie_cached_paths_purged =
        purge_zombie_cached_paths(&state, shard, &on_disk_set).await?;
    Ok(report)
}

fn zombie_candidates(shard: &str) -> sea_orm::Select<ECachedPath> {
    let last = format!("{shard}{}", "z".repeat(32usize.saturating_sub(shard.len())));
    ECachedPath::find()
        .filter(CCachedPath::Hash.between(shard.to_owned(), last))
        .filter(CCachedPath::FileHash.is_not_null())
        .filter(CCachedPath::Confirmed.eq(true))
}

/// The listing was taken before these rows were read. A NAR committed in between is absent from the
/// listing yet present in storage. Dropping its row would leave the producer `Completed` with no
/// backing output. Storage is the judge, and a probe error is preserving the row.
async fn zombie_hashes(
    state: &Arc<ServerState>,
    shard: &str,
    on_disk: &HashSet<String>,
) -> Result<Vec<String>> {
    let rows = zombie_candidates(shard)
        .all(&state.worker_db)
        .await
        .context("Failed to load cached_path rows for zombie purge")?;

    let mut zombies = Vec::new();
    for row in rows {
        if on_disk.contains(&row.hash) {
            continue;
        }

        match state.nar_storage.exists(&row.hash).await {
            Ok(false) => zombies.push(row.hash),
            Ok(true) => {
                debug!(hash = %row.hash, "zombie purge: the object landed after the listing")
            }
            Err(e) => {
                warn!(hash = %row.hash, error = %e, "zombie purge: could not probe the object")
            }
        }
    }

    Ok(zombies)
}

async fn purge_zombie_cached_paths(
    state: &Arc<ServerState>,
    shard: &str,
    on_disk: &HashSet<String>,
) -> Result<u64> {
    let zombies = zombie_hashes(state, shard, on_disk).await?;
    if zombies.is_empty() {
        return Ok(0);
    }

    const ZOMBIE_DELETE_BATCH: usize = 8000;
    let scanned_at = now();
    let mut purged = 0u64;
    for chunk in zombies.chunks(ZOMBIE_DELETE_BATCH) {
        match state
            .graph
            .gc(GcRequest::Paths {
                hashes: chunk.to_vec(),
                scanned_at,
            })
            .await
        {
            Ok(report) => purged += report.retired.len() as u64,
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

const UNREFERENCED_PROBE_BATCH: usize = 5000;

/// A fresh NAR is on disk before the evaluation committed its `derivation` and `cached_path` rows.
/// Reclaiming it would strand a zombie `cached_path` that the dispatch gate is trusting. A grace of
/// `<= 0` is for tests only.
fn past_upload_grace(state: &ServerState, on_disk: &[(String, i64)]) -> Vec<String> {
    let grace_secs = state.config.gc.nar_upload_grace_hours.max(0) * 3600;
    let cutoff = if grace_secs > 0 {
        now().and_utc().timestamp() - grace_secs
    } else {
        i64::MAX
    };
    on_disk
        .iter()
        .filter(|(_, modified)| *modified < cutoff)
        .map(|(hash, _)| hash.clone())
        .collect()
}

async fn unreferenced_hashes(state: &ServerState, candidates: &[String]) -> Result<Vec<String>> {
    let rows = state
        .worker_db
        .query_all_raw(UNREFERENCED_HASHES.bind([
            candidates.to_vec().into(),
            sea_orm::Value::Int(Some(BuildStatus::FailedPermanent as i32)),
            sea_orm::Value::Int(Some(BuildStatus::Aborted as i32)),
            sea_orm::Value::Int(Some(BuildStatus::DependencyFailed as i32)),
            sea_orm::Value::Int(Some(BuildStatus::FailedTimeout as i32)),
        ]))
        .await
        .context("Failed to probe NAR hashes for references")?;

    rows.iter()
        .map(|row| row.try_get::<String>("", "hash").map_err(Into::into))
        .collect()
}

async fn remove_orphan_nar(state: &ServerState, hash: &str) -> bool {
    match state.nar_storage.delete(hash).await {
        Ok(()) => {
            debug!(hash, "Removed orphaned NAR");
            true
        }
        Err(e) => {
            error!(hash, error = %e, "Failed to remove orphaned NAR");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_server_state;
    use gradient_storage::NarStore;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;
    use std::path::Path;

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

    fn hash_row(h: &str) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert("hash".to_string(), Value::String(Some(h.into())));
        m
    }

    fn make_state(base: &Path, unreferenced: Vec<&str>) -> Arc<ServerState> {
        let nar_storage = NarStore::local(base.to_str().unwrap()).unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([unreferenced.into_iter().map(hash_row).collect::<Vec<_>>()])
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();
        test_server_state(nar_storage, db, |config| {
            config.gc.nar_upload_grace_hours = 0;
        })
    }

    #[tokio::test]
    async fn removes_only_what_the_probe_names_unreferenced() {
        let tmp = tempfile::tempdir().unwrap();
        let active = "aabbccdd11111111111111111111111111";
        let orphan = "aaff001122222222222222222222222222";
        write_nar_file(tmp.path(), active);
        write_nar_file(tmp.path(), orphan);

        let state = make_state(tmp.path(), vec![orphan]);
        let report = repair_nar_shard(state, "aa").await.unwrap();

        assert!(nar_file_exists(tmp.path(), active));
        assert!(!nar_file_exists(tmp.path(), orphan));
        assert_eq!(report.orphan_nars_scanned, 2);
        assert_eq!(report.orphan_nars_removed, 1);
    }

    #[test]
    fn keep_clauses_protect_drv_and_sources_for_any_shared_build() {
        let sql = UNREFERENCED_HASHES
            .text()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            sql.matches("b.status NOT IN").count(),
            1,
            "only the outputs clause may gate on build status: {sql}"
        );
        assert!(
            sql.contains(
                "JOIN derivation_build b ON b.derivation = s.derivation WHERE s.hash = h.hash)"
            ),
            "input sources kept for any shared build (no status gate): {sql}"
        );
        assert!(
            sql.contains("JOIN derivation_build b ON b.derivation = d.id WHERE d.hash = h.hash)"),
            "drv kept for any shared build (no status gate): {sql}"
        );
    }

    #[test]
    fn the_probe_is_driven_by_the_listed_candidates() {
        let sql = UNREFERENCED_HASHES.text();
        assert!(sql.contains("FROM unnest($1::text[]) AS h(hash)"), "{sql}");
        assert_eq!(sql.matches("NOT EXISTS").count(), 4, "{sql}");
        assert!(!sql.contains("UNION"), "{sql}");
    }

    #[tokio::test]
    async fn a_referenced_nar_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let drv = "ddeeffaa33333333333333333333333333";
        write_nar_file(tmp.path(), drv);

        let state = make_state(tmp.path(), vec![]);
        repair_nar_shard(state, "dd").await.unwrap();

        assert!(nar_file_exists(tmp.path(), drv));
    }

    #[tokio::test]
    async fn every_unreferenced_candidate_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let h1 = "1111aaaa44444444444444444444444444";
        let h2 = "1122bbbb55555555555555555555555555";
        write_nar_file(tmp.path(), h1);
        write_nar_file(tmp.path(), h2);

        let state = make_state(tmp.path(), vec![h1, h2]);
        repair_nar_shard(state, "11").await.unwrap();

        assert!(!nar_file_exists(tmp.path(), h1));
        assert!(!nar_file_exists(tmp.path(), h2));
    }

    #[test]
    fn only_nars_past_the_upload_grace_are_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = make_state(tmp.path(), vec![]);
        Arc::make_mut(&mut Arc::get_mut(&mut state).unwrap().config)
            .gc
            .nar_upload_grace_hours = 24;
        let fresh = now().and_utc().timestamp();
        let listed = vec![
            ("old".to_owned(), fresh - 25 * 3600),
            ("fresh".to_owned(), fresh),
        ];

        assert_eq!(past_upload_grace(&state, &listed), vec!["old".to_owned()]);
    }

    #[tokio::test]
    async fn fresh_orphan_nar_spared_within_grace() {
        let tmp = tempfile::tempdir().unwrap();
        let orphan = "ddccbbaa99999999999999999999999999";
        write_nar_file(tmp.path(), orphan);

        let nar_storage = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let only_the_zombie_load = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();
        let state = test_server_state(nar_storage, only_the_zombie_load, |config| {
            config.gc.nar_upload_grace_hours = 24;
        });

        repair_nar_shard(state, "dd")
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
            package: "fresh".into(),
            file_hash: Some("sha256:beef".into()),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection();

        let nar_storage = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let state = test_server_state(nar_storage, db, |_| {});

        let zombies = zombie_hashes(&state, "cc", &HashSet::new()).await.unwrap();

        assert!(zombies.is_empty(), "storage decides, not the stale listing");
        assert!(nar_file_exists(tmp.path(), fresh));
    }

    #[tokio::test]
    async fn names_cached_paths_whose_nar_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let live = "aaaa11111111111111111111111111aaaa";
        let zombie_hash = "bbbb22222222222222222222222222bbbb";

        write_nar_file(tmp.path(), live);

        let nar_storage = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let rows = vec![
            gradient_entity::cached_path::Model {
                id: CachedPathId::now_v7(),
                hash: live.into(),
                package: "live".into(),
                file_hash: Some("sha256:live".into()),
                ..Default::default()
            },
            gradient_entity::cached_path::Model {
                id: CachedPathId::now_v7(),
                hash: zombie_hash.into(),
                package: "zombie".into(),
                file_hash: Some("sha256:zombie".into()),
                ..Default::default()
            },
        ];
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows])
            .into_connection();

        let state = test_server_state(nar_storage, db, |_| {});

        let zombies = zombie_hashes(&state, "", &HashSet::new()).await.unwrap();

        assert_eq!(zombies, vec![zombie_hash.to_owned()]);
        assert!(nar_file_exists(tmp.path(), live), "live NAR must survive");
    }

    #[test]
    fn the_zombie_purge_never_reads_an_unconfirmed_row() {
        use sea_orm::QueryTrait;
        let sql = zombie_candidates("0a")
            .build(DatabaseBackend::Postgres)
            .to_string()
            .to_uppercase();
        assert!(sql.contains(r#""CACHED_PATH"."CONFIRMED" = TRUE"#), "{sql}");
        assert!(
            sql.contains(r#""CACHED_PATH"."FILE_HASH" IS NOT NULL"#),
            "{sql}"
        );
    }
}
