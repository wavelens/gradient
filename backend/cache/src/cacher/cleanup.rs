/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::types::*;
use entity::build::BuildStatus;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, IntoActiveModel,
    QueryFilter, Statement,
};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

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
        if let Err(e) = gradient_core::db::gc_project_evaluations(Arc::clone(&state), project.id, keep).await
        {
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
                     SELECT 1 FROM build b
                     WHERE b.derivation = cd.derivation
                       AND b.status NOT IN ($2, $3, $4)
                 )
                 AND NOT EXISTS (
                     SELECT 1 FROM derivation_output do
                     WHERE do.derivation = cd.derivation
                       AND do.ca IS NOT NULL
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
                sea_orm::Value::Int(Some(BuildStatus::Failed as i32)),
                sea_orm::Value::Int(Some(BuildStatus::Aborted as i32)),
                sea_orm::Value::Int(Some(BuildStatus::DependencyFailed as i32)),
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

        // Find the outputs of the derivation; remove their NAR files (if no other
        // cache_derivation row keeps them alive).
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(drv_id))
            .all(&state.worker_db)
            .await
            .unwrap_or_default();

        // Drop the cache_derivation row first; revocation of dependents follows.
        if let Some(cd) = ECacheDerivation::find_by_id(cd_id)
            .one(&state.worker_db)
            .await
            .ok()
            .flatten()
        {
            let _ = cd.into_active_model().delete(&state.worker_db).await;
        }

        // Drop the cached_path_signature rows that tied each output to THIS
        // cache, and (if no other cache still references the path) the
        // cached_path row itself. Without this, the "compressed stored"
        // metric — a SUM(file_size) joined via cached_path_signature — would
        // stay inflated after TTL eviction even though the NAR file is gone.
        for o in &outputs {
            let cached_paths = ECachedPath::find()
                .filter(CCachedPath::Hash.eq(&o.hash))
                .all(&state.worker_db)
                .await
                .unwrap_or_default();

            for cp in cached_paths {
                let sigs_for_cache = ECachedPathSignature::find()
                    .filter(CCachedPathSignature::CachedPath.eq(cp.id))
                    .filter(CCachedPathSignature::Cache.eq(cache_id))
                    .all(&state.worker_db)
                    .await
                    .unwrap_or_default();
                for sig in sigs_for_cache {
                    let _ = sig.into_active_model().delete(&state.worker_db).await;
                }

                let still_signed = ECachedPathSignature::find()
                    .filter(CCachedPathSignature::CachedPath.eq(cp.id))
                    .one(&state.worker_db)
                    .await
                    .ok()
                    .flatten()
                    .is_some();
                if !still_signed {
                    let _ = cp.into_active_model().delete(&state.worker_db).await;
                }
            }
        }

        // Best-effort: NAR file is shared by every cache for this output, so only
        // delete when no cache_derivation row remains for the derivation.
        let still_held = ECacheDerivation::find()
            .filter(CCacheDerivation::Derivation.eq(drv_id))
            .one(&state.worker_db)
            .await
            .ok()
            .flatten()
            .is_some();
        if !still_held {
            for o in &outputs {
                if let Err(e) = state.nar_storage.delete(&o.hash).await {
                    warn!(error = %e, hash = %o.hash, "Failed to remove stale compressed NAR");
                }
            }
        }
    }

    Ok(())
}

pub async fn cleanup_orphaned_cache_files(state: Arc<ServerState>) -> Result<()> {
    let keep = active_hashes(&state).await?;

    let hashes = state
        .nar_storage
        .list_hashes()
        .await
        .context("Failed to list NAR store")?;

    let mut removed = 0usize;
    for hash in hashes {
        if keep.contains(&hash) {
            continue;
        }
        if let Err(e) = state.nar_storage.delete(&hash).await {
            error!(hash = %hash, error = %e, "Failed to remove orphaned NAR");
        } else {
            debug!(hash = %hash, "Removed orphaned NAR");
            removed += 1;
        }
    }

    if removed > 0 {
        info!(count = removed, "Removed orphaned NAR files");
    }

    Ok(())
}

/// Returns the set of NAR-storage hashes that must NOT be garbage-collected by
/// the orphan-files pass. A hash is kept when either:
///
/// 1. it belongs to a `derivation_output` whose `derivation` has at least one
///    `build` row whose status is not a terminal failure (`Failed`, `Aborted`,
///    `DependencyFailed`). This covers Substituted, Completed, and any
///    in-flight build (Created/Queued/Building) — including the upload race
///    window where the NAR is on disk before `is_cached=true` is flipped.
/// 2. it belongs to a `cached_path` row with `file_hash IS NOT NULL` —
///    typically `.drv` files that have no `derivation_output` of their own.
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
            r#"
            SELECT DISTINCT do.hash AS hash
            FROM derivation_output do
            JOIN build b ON b.derivation = do.derivation
            WHERE b.status NOT IN ($1, $2, $3)
            UNION
            SELECT cp.hash AS hash
            FROM cached_path cp
            WHERE cp.file_hash IS NOT NULL
            "#,
            [
                sea_orm::Value::Int(Some(BuildStatus::Failed as i32)),
                sea_orm::Value::Int(Some(BuildStatus::Aborted as i32)),
                sea_orm::Value::Int(Some(BuildStatus::DependencyFailed as i32)),
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
    use gradient_core::storage::{EmailSender, NarStore};
    use sea_orm::{MockDatabase, Value};
    use std::collections::BTreeMap;
    use std::path::Path;
    use test_support::fakes::email::InMemoryEmailSender;
    use test_support::fakes::webhooks::RecordingWebhookClient;
    use test_support::log_storage::NoopLogStorage;
    use test_support::prelude::test_cli;

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
            .into_connection();
        Arc::new(ServerState {
            web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            worker_db: WorkerDb::new(db),
            config: Arc::new(RuntimeConfig::from_cli(&test_cli())),
            log_storage: Arc::new(NoopLogStorage),
            webhooks: Arc::new(RecordingWebhookClient::new()),
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            http: gradient_core::http::build_client().expect("http client"),
            shutdown: gradient_core::shutdown::Shutdown::new(),
            jwt_secret: gradient_core::types::SecretString::new("test-jwt-secret".to_string()),
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

        assert!(nar_file_exists(tmp.path(), active), "active NAR must survive");
        assert!(!nar_file_exists(tmp.path(), orphan), "orphan NAR must be removed");
    }

    /// A NAR referenced only by a `cached_path` row (e.g. a `.drv` file) must
    /// be kept — exercises the UNION branch of the keep query.
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
        Arc::make_mut(&mut Arc::get_mut(&mut state).unwrap().config).storage.nar_ttl_hours = 0;

        cleanup_stale_cached_nars(state).await.unwrap();
        assert!(nar_file_exists(tmp.path(), h));
    }

    /// When the orphan-aware SELECT returns no `cache_derivation` rows, the
    /// TTL pass leaves on-disk NARs untouched — covering the case where
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
        let mut cli = test_cli();
        cli.storage.nar_ttl_hours = 24;
        let config = Arc::new(RuntimeConfig::from_cli(&cli));
        let state = Arc::new(ServerState {
            web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            worker_db: WorkerDb::new(db),
            config,
            log_storage: Arc::new(NoopLogStorage),
            webhooks: Arc::new(RecordingWebhookClient::new()),
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            http: gradient_core::http::build_client().expect("http client"),
            shutdown: gradient_core::shutdown::Shutdown::new(),
            jwt_secret: gradient_core::types::SecretString::new("test-jwt-secret".to_string()),
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
}
