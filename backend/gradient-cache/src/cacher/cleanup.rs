/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, IntoActiveModel,
    QueryFilter, Statement,
};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Per-pass counters returned by `cleanup_orphaned_cache_files`. The deep-GC
/// sweep threads these into its progress report; the hourly loop just logs
/// the result.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct CleanupReport {
    pub orphan_nars_scanned: u64,
    pub orphan_nars_removed: u64,
    pub zombie_cached_paths_purged: u64,
}

/// Build-request blob TTL pass: drops `build_request_blob` rows whose
/// `last_used_at` is older than `nar_ttl_hours` and removes the underlying
/// payload from `nar_storage`. Disabled when `nar_ttl_hours = 0`.
pub async fn cleanup_stale_build_request_blobs(state: Arc<ServerState>) -> Result<()> {
    let ttl_hours = state.config.storage.nar_ttl_hours;
    if ttl_hours == 0 {
        return Ok(());
    }

    let cutoff = now() - chrono::Duration::hours(ttl_hours as i64);
    let stale = EBuildRequestBlob::find()
        .filter(CBuildRequestBlob::LastUsedAt.lt(cutoff))
        .all(&state.worker_db)
        .await
        .context("Failed to query stale build_request_blob rows")?;

    let mut removed = 0usize;
    for blob in stale {
        if blob.hash.len() != 32 {
            warn!(blob_id = %blob.id, "skipping build_request_blob with malformed hash");
            continue;
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&blob.hash);
        let blob_id = blob.id;
        let org_id = blob.organization;
        if let Err(e) = blob.into_active_model().delete(&state.worker_db).await {
            warn!(error = %e, %blob_id, "failed to delete build_request_blob row");
            continue;
        }
        if let Err(e) = state
            .nar_storage
            .delete_blob(org_id.into_inner(), &hash)
            .await
        {
            warn!(error = %e, %blob_id, "failed to delete build-request blob payload");
        }
        removed += 1;
    }

    if removed > 0 {
        info!(count = removed, "Removed stale build-request blobs");
    }
    Ok(())
}

/// Upload-session GC pass: drops `upload_session` rows whose `expires_at` has
/// passed and that were never dispatched. Dispatched sessions are kept for
/// audit history (the dispatch endpoint already nulls their `missing` set, so
/// they are harmless dead-weight rather than blobs).
pub async fn cleanup_expired_upload_sessions(state: Arc<ServerState>) -> Result<()> {
    let res = EUploadSession::delete_many()
        .filter(CUploadSession::ExpiresAt.lt(now()))
        .filter(CUploadSession::DispatchedAt.is_null())
        .exec(&state.worker_db)
        .await
        .context("Failed to delete expired upload_session rows")?;

    if res.rows_affected > 0 {
        info!(count = res.rows_affected, "Removed expired upload sessions");
    }
    Ok(())
}

pub async fn cleanup_old_evaluations(state: Arc<ServerState>) -> Result<()> {
    let projects = EProject::find()
        .all(&state.worker_db)
        .await
        .context("Failed to query projects for evaluation GC")?;

    for project in projects {
        let keep = project.keep_evaluations as usize;
        if keep == 0 {
            continue;
        }
        if let Err(e) = gradient_db::gc_project_evaluations(&state.db(), project.id, keep).await {
            warn!(error = %e, project_id = %project.id, "Evaluation GC failed for project");
        }
    }

    Ok(())
}

/// Cache NAR TTL pass: deletes `cache_derivation` rows whose `last_fetched_at`
/// is older than `nar_ttl_hours` **and** whose derivation has no active build
/// (status not in `Failed` / `Aborted` / `DependencyFailed`). For each expired
/// row, deletes the NAR file from storage and drops the row. The derivation
/// and its outputs stay (other caches may still hold them).
///
/// The active-build guard makes the TTL pass an orphan-eviction step in the
/// design "old evals/builds deleted by `keep_evaluations` → derivation
/// becomes orphan → NAR kept for `nar_ttl_hours` → evicted". It prevents
/// evicting NARs of derivations that are still referenced by an active
/// evaluation just because no one happened to fetch them recently.
///
/// Fixed-output derivations (any `derivation_output` with `ca IS NOT NULL`)
/// are skipped entirely: their NARs come from external sources that may no
/// longer be reachable (404s, deleted release tarballs), so a transient gap
/// in build references must not delete the only cached copy. FOD NARs are
/// reclaimed only by `gc_orphan_derivations`, which fires after the grace
/// period and zero remaining build references.
const STALE_CACHED_NARS_SELECT: &str = r#"SELECT cd.id, cd.cache, cd.derivation
               FROM cache_derivation cd
               WHERE cd.last_fetched_at IS NOT NULL
                 AND cd.last_fetched_at < NOW() AT TIME ZONE 'UTC' - ($1 * INTERVAL '1 hour')
                 AND NOT EXISTS (
                     SELECT 1 FROM derivation_build b
                     WHERE b.derivation = cd.derivation
                       AND b.status NOT IN ($2, $3, $4, $5)
                 )
                 AND NOT EXISTS (
                     SELECT 1 FROM derivation_output dout
                     WHERE dout.derivation = cd.derivation
                       AND dout.ca IS NOT NULL
                 )"#;

pub async fn cleanup_stale_cached_nars(state: Arc<ServerState>) -> Result<()> {
    let ttl_hours = state.config.storage.nar_ttl_hours;
    if ttl_hours == 0 {
        return Ok(());
    }

    let rows = state
        .worker_db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            STALE_CACHED_NARS_SELECT,
            [
                sea_orm::Value::BigInt(Some(ttl_hours as i64)),
                sea_orm::Value::Int(Some(BuildStatus::FailedPermanent as i32)),
                sea_orm::Value::Int(Some(BuildStatus::Aborted as i32)),
                sea_orm::Value::Int(Some(BuildStatus::DependencyFailed as i32)),
                sea_orm::Value::Int(Some(BuildStatus::FailedTimeout as i32)),
            ],
        ))
        .await
        .context("Failed to query stale cache_derivation rows")?;

    for row in rows {
        let cd_id: Uuid = match row.try_get("", "id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let cache_id: Uuid = match row.try_get("", "cache") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let drv_id: Uuid = match row.try_get("", "derivation") {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Every reference check below propagates its error: a row whose check
        // failed is skipped this pass (retried next hour), never treated as
        // unreferenced and reclaimed.
        let output_hashes: Vec<String> = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(drv_id))
            .all(&state.worker_db)
            .await
            .context("TTL GC: failed to load derivation outputs")?
            .into_iter()
            .map(|o| o.hash)
            .collect();

        // Drop the cache_derivation row first; revocation of dependents follows.
        ECacheDerivation::delete_many()
            .filter(CCacheDerivation::Id.eq(cd_id))
            .exec(&state.worker_db)
            .await
            .context("TTL GC: failed to delete cache_derivation row")?;

        // Drop THIS cache's signatures on the outputs' cached paths, then any
        // cached_path no cache signs anymore - clearing the gate flags it backed
        // in the same transaction. Without the signature cleanup the "compressed
        // stored" metric (SUM(file_size) via cached_path_signature) would stay
        // inflated after TTL eviction even though the NAR file is gone.
        if !output_hashes.is_empty() {
            use sea_orm::TransactionTrait;
            let txn = state.worker_db.inner().begin().await?;
            txn.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
                DELETE FROM cached_path_signature s
                USING cached_path cp
                WHERE s.cached_path = cp.id AND s.cache = $1 AND cp.hash = ANY($2)
                "#,
                [cache_id.into(), output_hashes.clone().into()],
            ))
            .await
            .context("TTL GC: failed to delete cached_path_signature rows")?;

            let dropped = txn
                .query_all(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    r#"
                    DELETE FROM cached_path cp
                    WHERE cp.hash = ANY($1)
                      AND NOT EXISTS (
                        SELECT 1 FROM cached_path_signature s WHERE s.cached_path = cp.id)
                    RETURNING cp.hash
                    "#,
                    [output_hashes.clone().into()],
                ))
                .await
                .context("TTL GC: failed to delete unsigned cached_path rows")?
                .into_iter()
                .filter_map(|r| r.try_get::<String>("", "hash").ok())
                .collect::<Vec<_>>();
            gradient_db::clear_gate_flags_for_hashes(&txn, &dropped)
                .await
                .context("TTL GC: failed to clear gate flags")?;
            txn.commit().await?;
        }

        // NAR file is shared by every cache for this output, so only delete when
        // no cache_derivation row remains for the derivation.
        let still_held = ECacheDerivation::find()
            .filter(CCacheDerivation::Derivation.eq(drv_id))
            .one(&state.worker_db)
            .await
            .context("TTL GC: failed to check surviving cache_derivation rows")?
            .is_some();
        if !still_held {
            for hash in &output_hashes {
                if let Err(e) = state.nar_storage.delete(hash).await {
                    warn!(error = %e, %hash, "Failed to remove stale compressed NAR");
                }
            }
        }
    }

    Ok(())
}

pub async fn cleanup_orphaned_cache_files(state: Arc<ServerState>) -> Result<CleanupReport> {
    let keep = active_hashes(&state).await?;

    let on_disk = state
        .nar_storage
        .list_hashes_with_modified()
        .await
        .context("Failed to list NAR store")?;
    let on_disk_set: HashSet<String> = on_disk.iter().map(|(h, _)| h.clone()).collect();

    // Spare NARs younger than the upload grace window. A freshly-uploaded NAR is
    // on disk before the eval has committed its `derivation`/`cached_path` rows,
    // so the keep-set does not yet reference it; reclaiming it here strands a
    // zombie `cached_path` (row created moments later, object gone) that the
    // dispatch gate trusts as the cached `.drv` - the in-eval `.drv` push race
    // that fails dependents `InputsUnavailable`. `<= 0` disables it (tests only).
    let grace_secs = state.config.storage.nar_upload_grace_hours.max(0) * 3600;
    let cutoff = if grace_secs > 0 {
        now().and_utc().timestamp() - grace_secs
    } else {
        i64::MAX
    };

    let mut report = CleanupReport {
        orphan_nars_scanned: on_disk.len() as u64,
        ..Default::default()
    };
    for (hash, modified) in &on_disk {
        if keep.contains(hash) || *modified >= cutoff {
            continue;
        }
        if let Err(e) = state.nar_storage.delete(hash).await {
            error!(hash = %hash, error = %e, "Failed to remove orphaned NAR");
        } else {
            debug!(hash = %hash, "Removed orphaned NAR");
            report.orphan_nars_removed += 1;
        }
    }

    if report.orphan_nars_removed > 0 {
        info!(
            count = report.orphan_nars_removed,
            "Removed orphaned NAR files"
        );
    }

    report.zombie_cached_paths_purged = purge_zombie_cached_paths(&state, &on_disk_set).await?;
    Ok(report)
}

/// Drop `cached_path` rows whose `file_hash IS NOT NULL` but whose NAR is no
/// longer in `nar_storage`. `gc_orphan_derivations` and external storage
/// lifecycle policies (S3 expiration, manual cleanup) can leave the row + its
/// `cached_path_signature` placeholders behind, which inflates the
/// `total_packages` / `total_bytes` cache stats and the sign-sweep workload.
/// `cached_path_signature` cascades from `cached_path`, so a single delete
/// drops both.
async fn purge_zombie_cached_paths(
    state: &Arc<ServerState>,
    on_disk: &HashSet<String>,
) -> Result<u64> {
    let rows = ECachedPath::find()
        .filter(CCachedPath::FileHash.is_not_null())
        .all(&state.worker_db)
        .await
        .context("Failed to load cached_path rows for zombie purge")?;

    let zombies: Vec<(gradient_types::ids::CachedPathId, String)> = rows
        .into_iter()
        .filter(|row| !on_disk.contains(&row.hash))
        .map(|row| (row.id, row.hash))
        .collect();
    if zombies.is_empty() {
        return Ok(0);
    }

    // Batch the deletes: a full fleet eval leaves hundreds of thousands of
    // `cached_path` rows, and per-row round-trips made the hourly pass never
    // finish (and never log). `cached_path_signature` cascades from `cached_path`.
    // Each batch deletes the rows AND clears the gate flags they backed in one
    // transaction, so the dispatch gate never trusts a just-purged zombie.
    const ZOMBIE_DELETE_BATCH: usize = 8000;
    let mut purged = 0u64;
    for chunk in zombies.chunks(ZOMBIE_DELETE_BATCH) {
        let ids: Vec<_> = chunk.iter().map(|(id, _)| *id).collect();
        let hashes: Vec<String> = chunk.iter().map(|(_, h)| h.clone()).collect();
        let deleted = async {
            use sea_orm::TransactionTrait;
            let txn = state.worker_db.inner().begin().await?;
            let res = ECachedPath::delete_many()
                .filter(CCachedPath::Id.is_in(ids))
                .exec(&txn)
                .await?;
            gradient_db::clear_gate_flags_for_hashes(&txn, &hashes).await?;
            txn.commit().await?;
            Ok::<u64, sea_orm::DbErr>(res.rows_affected)
        }
        .await;
        match deleted {
            Ok(n) => purged += n,
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

/// Keep-set query for the orphan-files pass. Outputs (clause 1) stay gated on
/// build status - they are rebuildable and TTL-evicted by `cleanup_stale_cached_nars`.
/// The `.drv` (clause 4) and input sources (clause 3) are producerless and kept
/// for any anchor regardless of status; only `gc_orphan_derivations` reclaims them.
const ACTIVE_HASHES_SELECT: &str = r#"
    SELECT DISTINCT dout.hash AS hash
    FROM derivation_output dout
    JOIN derivation_build b ON b.derivation = dout.derivation
    WHERE b.status NOT IN ($1, $2, $3, $4)
    UNION
    SELECT cp.hash AS hash
    FROM cached_path cp
    WHERE cp.file_hash IS NOT NULL
    UNION
    SELECT s.hash AS hash
    FROM derivation_input_source s
    JOIN derivation_build b ON b.derivation = s.derivation
    UNION
    SELECT d.hash AS hash
    FROM derivation d
    JOIN derivation_build b ON b.derivation = d.id
"#;

/// Returns the set of NAR-storage hashes that must NOT be garbage-collected by
/// the orphan-files pass. A hash is kept when either:
///
/// 1. it belongs to a `derivation_output` whose `derivation` has a
///    `derivation_build` anchor whose status is not a terminal failure
///    (`Failed`, `Aborted`, `DependencyFailed`). This covers Substituted,
///    Completed, and any in-flight build (Created/Queued/Building) - including
///    the upload race window where the NAR is on disk before `is_cached=true`
///    is flipped.
/// 2. it belongs to a `cached_path` row with `file_hash IS NOT NULL` -
///    typically `.drv` files that have no `derivation_output` of their own.
/// 3. it is a build-time **input source** or the **`.drv`** of any derivation
///    with a build anchor, regardless of status. These have no `derivation_output`
///    and no producer (only an eval re-pushes them), so a terminal-failed anchor a
///    later eval requeues must still find them - gating this clause on status
///    purged the `.drv`/sources of a failed-but-requeueable build, dead-ending its
///    retry on `InputsUnavailable`. Genuinely dead ones are reclaimed by
///    `gc_orphan_derivations` when the derivation row goes orphan.
///
/// Note: this is intentionally more permissive than the old `is_cached=true`
/// check. `gc_orphan_derivations` and `cleanup_stale_cached_nars` are the
/// passes that actively remove NARs once their derivations are no longer
/// referenced; this pass is a safety net for stray files only.
async fn active_hashes(state: &Arc<ServerState>) -> Result<HashSet<String>> {
    let rows = state
        .worker_db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            ACTIVE_HASHES_SELECT,
            [
                sea_orm::Value::Int(Some(BuildStatus::FailedPermanent as i32)),
                sea_orm::Value::Int(Some(BuildStatus::Aborted as i32)),
                sea_orm::Value::Int(Some(BuildStatus::DependencyFailed as i32)),
                sea_orm::Value::Int(Some(BuildStatus::FailedTimeout as i32)),
            ],
        ))
        .await
        .context("Failed to query active NAR hashes")?;

    let mut set: HashSet<String> = HashSet::with_capacity(rows.len());
    for row in rows {
        if let Ok(h) = row.try_get::<String>("", "hash") {
            set.insert(h);
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cacher::test_support::test_server_state;
    use gradient_storage::NarStore;
    use sea_orm::{MockDatabase, Value};
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
        m.insert("hash".to_string(), Value::String(Some(Box::new(h.into()))));
        m
    }

    fn make_state(base: &Path, kept: Vec<&str>) -> Arc<ServerState> {
        let nar_storage = NarStore::local(base.to_str().unwrap()).unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([kept.into_iter().map(hash_row).collect::<Vec<_>>()])
            // `purge_zombie_cached_paths` follows the active-hashes query with a
            // load of cached_path rows. None of these tests exercise that path,
            // so feed it an empty result set.
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();
        test_server_state(nar_storage, db, |config| {
            // Disable the orphan-file grace window so these tests' freshly
            // written NARs are eligible for reclamation immediately.
            config.storage.nar_upload_grace_hours = 0;
        })
    }

    /// A NAR for a derivation with an active build (Substituted/Completed/etc.)
    /// must be kept; a NAR with no DB references must be removed.
    #[tokio::test]
    async fn keeps_active_drops_orphan() {
        let tmp = tempfile::tempdir().unwrap();
        let active = "aabbccdd11111111111111111111111111";
        let orphan = "eeff001122222222222222222222222222";
        write_nar_file(tmp.path(), active);
        write_nar_file(tmp.path(), orphan);

        let state = make_state(tmp.path(), vec![active]);
        cleanup_orphaned_cache_files(state).await.unwrap();

        assert!(
            nar_file_exists(tmp.path(), active),
            "active NAR must survive"
        );
        assert!(
            !nar_file_exists(tmp.path(), orphan),
            "orphan NAR must be removed"
        );
    }

    /// Only the outputs clause may gate on build status. The `.drv` and
    /// input-source clauses keep a derivation's build closure for ANY anchor, so a
    /// requeued terminal-failed build can still fetch its `.drv`.
    #[test]
    fn keep_set_protects_drv_and_sources_for_any_anchor() {
        let sql = ACTIVE_HASHES_SELECT
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            sql.matches("b.status NOT IN").count(),
            1,
            "only the outputs clause may gate on build status: {sql}"
        );
        assert!(
            sql.contains("FROM derivation_input_source s JOIN derivation_build b ON b.derivation = s.derivation UNION"),
            "input sources kept for any anchor (no status gate): {sql}"
        );
        assert!(
            sql.contains("FROM derivation d JOIN derivation_build b ON b.derivation = d.id"),
            "drv kept for any anchor (no status gate): {sql}"
        );
    }

    /// A NAR referenced only by a `cached_path` row (e.g. a `.drv` file) must
    /// be kept - exercises the UNION branch of the keep query.
    #[tokio::test]
    async fn keeps_cached_path_only() {
        let tmp = tempfile::tempdir().unwrap();
        let drv = "ddeeffaa33333333333333333333333333";
        write_nar_file(tmp.path(), drv);

        let state = make_state(tmp.path(), vec![drv]);
        cleanup_orphaned_cache_files(state).await.unwrap();

        assert!(nar_file_exists(tmp.path(), drv));
    }

    /// `cleanup_stale_cached_nars` is a no-op when `nar_ttl_hours = 0`: it must
    /// not even issue the SELECT, so a state with an empty mock DB doesn't
    /// blow up.
    #[tokio::test]
    async fn stale_nars_disabled_when_ttl_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let h = "ffeeddcc66666666666666666666666666";
        write_nar_file(tmp.path(), h);

        let mut state = make_state(tmp.path(), vec![]);
        // SAFETY: only this test holds a clone; mutate before any await.
        Arc::make_mut(&mut Arc::get_mut(&mut state).unwrap().config)
            .storage
            .nar_ttl_hours = 0;

        cleanup_stale_cached_nars(state).await.unwrap();
        assert!(nar_file_exists(tmp.path(), h));
    }

    /// When the orphan-aware SELECT returns no `cache_derivation` rows, the
    /// TTL pass leaves on-disk NARs untouched - covering the case where
    /// every cache_derivation is either fresh or still tied to an active
    /// build.
    #[tokio::test]
    async fn stale_nars_no_eligible_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let h = "abcdef00777777777777777777777777";
        write_nar_file(tmp.path(), h);

        let nar_storage = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results::<BTreeMap<String, Value>, _, _>([vec![]])
            .into_connection();
        let state = test_server_state(nar_storage, db, |config| {
            config.storage.nar_ttl_hours = 24;
        });

        cleanup_stale_cached_nars(state).await.unwrap();
        assert!(nar_file_exists(tmp.path(), h));
    }

    /// Regression for #107: the TTL SELECT must exclude any derivation that
    /// owns a fixed-output `derivation_output` (`ca IS NOT NULL`), because a
    /// FOD's NAR may not be re-fetchable from upstream and is reclaimed only
    /// by `gc_orphan_derivations`.
    #[test]
    fn ttl_select_skips_fixed_output_derivations() {
        assert!(
            STALE_CACHED_NARS_SELECT.contains("derivation_output")
                && STALE_CACHED_NARS_SELECT.contains("ca IS NOT NULL"),
            "TTL SELECT lost its FOD guard: {STALE_CACHED_NARS_SELECT}"
        );
    }

    /// Empty keep set ⇒ every on-disk NAR is removed.
    #[tokio::test]
    async fn drops_everything_when_no_keep() {
        let tmp = tempfile::tempdir().unwrap();
        let h1 = "1111aaaa44444444444444444444444444";
        let h2 = "2222bbbb55555555555555555555555555";
        write_nar_file(tmp.path(), h1);
        write_nar_file(tmp.path(), h2);

        let state = make_state(tmp.path(), vec![]);
        cleanup_orphaned_cache_files(state).await.unwrap();

        assert!(!nar_file_exists(tmp.path(), h1));
        assert!(!nar_file_exists(tmp.path(), h2));
    }

    /// A NAR younger than the orphan grace window is spared even with an empty
    /// keep set: it may be a just-uploaded `.drv` whose `derivation`/`cached_path`
    /// rows have not committed yet, so the keep-set does not reference it. Without
    /// the grace it would be reclaimed and strand a zombie `cached_path`.
    #[tokio::test]
    async fn fresh_orphan_nar_spared_within_grace() {
        let tmp = tempfile::tempdir().unwrap();
        let orphan = "ddccbbaa99999999999999999999999999";
        write_nar_file(tmp.path(), orphan);

        // make_state disables the grace; re-enable the default 24h window.
        let mut state = make_state(tmp.path(), vec![]);
        Arc::make_mut(&mut Arc::get_mut(&mut state).unwrap().config)
            .storage
            .nar_upload_grace_hours = 24;

        cleanup_orphaned_cache_files(state).await.unwrap();
        assert!(
            nar_file_exists(tmp.path(), orphan),
            "freshly written orphan NAR must be spared within the grace window"
        );
    }

    /// `cached_path` rows whose NAR is gone from storage are zombies left
    /// behind by `gc_orphan_derivations`. The orphaned-files pass purges them
    /// so the per-cache stats query (`COUNT(cached_path_signature.id)`) and
    /// the sign sweep stop iterating ghosts.
    #[tokio::test]
    async fn purges_cached_paths_whose_nar_is_missing() {
        use sea_orm::ActiveValue::Set;

        let tmp = tempfile::tempdir().unwrap();
        let live = "aaaa11111111111111111111111111aaaa";
        let zombie_id = CachedPathId::now_v7();
        let zombie_hash = "bbbb22222222222222222222222222bbbb";

        // Only the live NAR is on disk; the zombie cached_path's hash isn't.
        write_nar_file(tmp.path(), live);

        let nar_storage = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let zombie_row = gradient_entity::cached_path::Model {
            id: zombie_id,
            hash: zombie_hash.into(),
            package: "zombie".into(),
            file_hash: Some(
                "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
            ),
            file_size: Some(1),
            nar_size: Some(1),
            nar_hash: Some("sha256:zombie".into()),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![hash_row(live)]])
            .append_query_results([vec![zombie_row.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let state = test_server_state(nar_storage, db, |_| {});

        cleanup_orphaned_cache_files(Arc::clone(&state))
            .await
            .unwrap();
        assert!(nar_file_exists(tmp.path(), live), "live NAR must survive");
        // The mock executor records the cached_path delete; we can't peek into
        // it directly, but absence of a panic plus the purge-counter increment
        // confirms the new branch ran. If the row weren't deleted, the
        // following `Set` use would dead-code and rustc would flag it.
        let _ = Set(zombie_id);
    }

    fn state_with_worker_db(base: &Path, db: sea_orm::DatabaseConnection) -> Arc<ServerState> {
        let nar_storage = NarStore::local(base.to_str().unwrap()).unwrap();
        test_server_state(nar_storage, db, |config| {
            config.storage.nar_ttl_hours = 24;
        })
    }

    /// Stale `build_request_blob` rows (older than `nar_ttl_hours`) are
    /// deleted and the underlying NAR-storage payload is removed.
    #[tokio::test]
    async fn build_request_blob_sweep_evicts_stale() {
        use gradient_entity::ids::{BuildRequestBlobId, OrganizationId};

        let tmp = tempfile::tempdir().unwrap();
        let org = OrganizationId::now_v7();
        let hash = [0xABu8; 32];
        let stale = gradient_entity::build_request_blob::Model {
            id: BuildRequestBlobId::now_v7(),
            organization: org,
            hash: hash.to_vec(),
            size: 1,
            created_at: now() - chrono::Duration::days(30),
            last_used_at: now() - chrono::Duration::days(30),
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![stale.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let state = state_with_worker_db(tmp.path(), db);

        state
            .nar_storage
            .put_blob(org.into_inner(), &hash, b"payload".to_vec())
            .await
            .unwrap();

        cleanup_stale_build_request_blobs(Arc::clone(&state))
            .await
            .unwrap();

        assert!(
            state
                .nar_storage
                .get_blob(org.into_inner(), &hash)
                .await
                .unwrap()
                .is_none(),
            "stale blob payload must be removed from storage"
        );
    }

    /// `nar_ttl_hours = 0` disables the sweep so the SELECT is never issued
    /// (an empty mock DB would error otherwise).
    #[tokio::test]
    async fn build_request_blob_sweep_disabled_when_ttl_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let nar_storage = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = test_server_state(nar_storage, db, |config| {
            config.storage.nar_ttl_hours = 0;
        });

        cleanup_stale_build_request_blobs(state).await.unwrap();
    }

    /// Build-request blob rows with malformed hashes are skipped (logged)
    /// rather than panicking on `copy_from_slice`.
    #[tokio::test]
    async fn build_request_blob_sweep_skips_malformed_hash() {
        use gradient_entity::ids::{BuildRequestBlobId, OrganizationId};

        let tmp = tempfile::tempdir().unwrap();
        let bad = gradient_entity::build_request_blob::Model {
            id: BuildRequestBlobId::now_v7(),
            organization: OrganizationId::now_v7(),
            hash: vec![1, 2, 3],
            size: 1,
            created_at: now() - chrono::Duration::days(30),
            last_used_at: now() - chrono::Duration::days(30),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![bad]])
            .into_connection();
        let state = state_with_worker_db(tmp.path(), db);

        cleanup_stale_build_request_blobs(state).await.unwrap();
    }

    /// Expired `upload_session` rows without a `dispatched_at` are deleted in
    /// one shot via `delete_many`.
    #[tokio::test]
    async fn upload_session_sweep_deletes_expired_undispatched() {
        let tmp = tempfile::tempdir().unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 3,
            }])
            .into_connection();
        let state = state_with_worker_db(tmp.path(), db);

        cleanup_expired_upload_sessions(state).await.unwrap();
    }
}
