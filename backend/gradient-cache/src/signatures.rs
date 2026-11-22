/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::WorkerDb;
use gradient_db::graph::reachability::sql_condition_for_output_that_must_not_be_signed;
use gradient_db::lookup::by_ids;
use gradient_db::store_path_content_variant_trust::{
    build_sql_for_content_variants_needing_signature_from_store_hash_project_seeds,
    cache_can_sign_content_variant_sql_condition,
};
use gradient_graph::Graph;
use gradient_sources::CacheSigner;
use gradient_types::*;
use gradient_util::nix_hash::normalize_nar_hash;
use gradient_util::shutdown::Shutdown;
use gradient_util::supervision::{Pass, PassError};
use gradient_util::sync::Mutex;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, TransactionTrait};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tracing::{debug, warn};

const SIGN_SWEEP_BATCH: u64 = 1000;

const SIGN_SWEEP_MAXIMUM_WAIT_BETWEEN_RUNS: Duration = Duration::from_secs(300);

const DEPENDENCY_OUTPUT_PUBLISH_ROUND_DEADLINE: Duration = Duration::from_secs(150);

const SIGN_SWEEP_PUBLISH_MINIMUM_TIME_BETWEEN_STARTS: Duration = Duration::from_secs(2);

/// A wake can start a run. That run can begin the publish step at most once per SIGN_SWEEP_PUBLISH_MINIMUM_TIME_BETWEEN_STARTS. An early wake can schedule one notification for the end of that interval. The server shutdown can track this as a task. Those wakes arrive late. They never get lost. A server shutdown can happen first. That's the exception. The task can end right away without a notification.
pub(crate) struct SignatureSweepPublishStartAndPendingWake {
    last_publish_step_started_at: Mutex<Option<tokio::time::Instant>>,
    signature_sweep_delayed_wake_scheduled: Arc<AtomicBool>,
    server_shutdown: Shutdown,
}

impl SignatureSweepPublishStartAndPendingWake {
    pub(crate) fn new(server_shutdown: Shutdown) -> Self {
        Self {
            last_publish_step_started_at: Mutex::new(None),
            signature_sweep_delayed_wake_scheduled: Arc::default(),
            server_shutdown,
        }
    }

    fn try_start_publish_or_schedule_next_publish_wake(
        &self,
        woken: bool,
        wake: &Arc<Notify>,
    ) -> bool {
        let now = tokio::time::Instant::now();
        let mut last_publish_step_started_at = self.last_publish_step_started_at.lock();
        let next_publish_allowed_at = last_publish_step_started_at
            .map(|started| started + SIGN_SWEEP_PUBLISH_MINIMUM_TIME_BETWEEN_STARTS)
            .filter(|allowed_at| woken && now < *allowed_at);
        let Some(next_publish_allowed_at) = next_publish_allowed_at else {
            *last_publish_step_started_at = Some(now);
            return true;
        };
        if !self
            .signature_sweep_delayed_wake_scheduled
            .swap(true, Ordering::AcqRel)
        {
            let scheduled = Arc::clone(&self.signature_sweep_delayed_wake_scheduled);
            let wake = Arc::clone(wake);
            let shutdown_token = self.server_shutdown.token();
            self.server_shutdown.spawn(async move {
                tokio::select! {
                    biased;
                    _ = shutdown_token.cancelled() => {}
                    _ = tokio::time::sleep_until(next_publish_allowed_at) => {
                        scheduled.store(false, Ordering::Release);
                        wake.notify_one();
                    }
                }
            });
        }
        false
    }
}

gradient_db::sql_fn! {
    /// A NAR committed without a resolvable owning job got no cache claim. The signing pass is only
    /// filling existing rows and cannot repair that. Such a path would 404 from the narinfo
    /// endpoint forever. The anti-join is keeping a healthy instance at one indexed probe.
    REPAIR_ORPHAN_CLAIMS = || format!(
        r#"
INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at)
SELECT uuidv7(), claimable.cached_path, claimable.cache, 0, (now() AT TIME ZONE 'UTC')
FROM ({claimable}) claimable
ON CONFLICT DO NOTHING
"#,
        claimable = build_sql_for_content_variants_needing_signature_from_store_hash_project_seeds(
            "SELECT DISTINCT orphan.hash, t.project \
             FROM (SELECT cp.hash FROM cached_path cp \
                   WHERE NOT EXISTS ( \
                       SELECT 1 FROM cached_path_signature s WHERE s.cached_path = cp.id) \
                   LIMIT $1) orphan \
             JOIN derivation_output dout ON dout.hash = orphan.hash \
             JOIN build_job bj           ON bj.derivation = dout.derivation \
             JOIN evaluation e           ON e.id = bj.evaluation \
             JOIN task t                 ON t.id = e.task"
        ),
    ),
        params = [Int(1000)],
        tier = Sweep;

    /// Use OFFSET 0 to keep the check for outputs that get no signature inside a subquery. Such subqueries run once per candidate row, so batch cost is proportional to batch size.
    PENDING_SIGNATURES_THE_CACHE_MAY_SIGN = || format!(
        "SELECT s.* FROM cached_path_signature s \
         WHERE s.signature IS NULL AND {accepted} \
           AND EXISTS (SELECT 1 FROM cache keyed_cache \
                       WHERE keyed_cache.id = s.cache AND keyed_cache.private_key <> '') \
           AND NOT EXISTS (SELECT 1 FROM cached_path pending_path \
                           JOIN derivation_output pending_output ON pending_output.hash = pending_path.hash \
                           WHERE pending_path.id = s.cached_path AND {must_not_be_signed} \
                           OFFSET 0) \
         ORDER BY s.id \
         LIMIT $1",
        accepted = cache_can_sign_content_variant_sql_condition("s.cached_path", "s.cache"),
        must_not_be_signed = sql_condition_for_output_that_must_not_be_signed("pending_output"),
    ),
        params = [Int(1000)],
        tier = Sweep;
}

pub(crate) struct SignSweep {
    pub(crate) db: WorkerDb,
    pub(crate) graph: Arc<Graph>,
    pub(crate) config: Arc<RuntimeConfig>,
    pub(crate) store_path_mapping: Arc<dyn StorePathMapping>,
    pub(crate) period: Duration,
    pub(crate) wake: Arc<Notify>,
    pub(crate) last_signing_pass_handled_parent_build_derivation_id: Mutex<Option<DerivationId>>,
    pub(crate) last_signing_started_at: Mutex<Option<Instant>>,
    pub(crate) last_complete_sweep_finished_at: Mutex<Option<Instant>>,
    pub(crate) publish_start_and_scheduled_delayed_wake: SignatureSweepPublishStartAndPendingWake,
}

impl Pass for SignSweep {
    const NAME: &'static str = "sign-sweep";
    const BUDGET: Duration = Duration::from_secs(300);

    fn period(&self) -> Duration {
        self.period.min(SIGN_SWEEP_MAXIMUM_WAIT_BETWEEN_RUNS)
    }

    fn wake(&self) -> Option<Arc<Notify>> {
        Some(Arc::clone(&self.wake))
    }

    async fn run(&self) -> Result<(), PassError> {
        // Supervisor runs finish with a sleep of one full period. Runs that start inside that period
        // are wakes. Wake events always trigger a signature. Timer runs sign only at the end of the
        // signing interval.
        let woken = self
            .last_complete_sweep_finished_at
            .lock()
            .is_some_and(|finished| finished.elapsed() < self.period());
        let signing_due = woken
            || self
                .last_signing_started_at
                .lock()
                .is_none_or(|started| started.elapsed() >= self.period);
        let publish = if self
            .publish_start_and_scheduled_delayed_wake
            .try_start_publish_or_schedule_next_publish_wake(woken, &self.wake)
        {
            self.publish_dependency_outputs_and_save_cursor().await
        } else {
            Ok(())
        };
        let sign = if signing_due {
            *self.last_signing_started_at.lock() = Some(Instant::now());
            self.sign().await
        } else {
            Ok(())
        };
        *self.last_complete_sweep_finished_at.lock() = Some(Instant::now());
        if let (Err(_), Err(sign_error)) = (&publish, &sign) {
            warn!(
                error = %sign_error,
                "sign sweep: signing failed after publish of dependency outputs failed too"
            );
        }
        publish.and(sign).map_err(Into::into)
    }
}

impl SignSweep {
    async fn publish_dependency_outputs_and_save_cursor(&self) -> anyhow::Result<()> {
        let start = *self
            .last_signing_pass_handled_parent_build_derivation_id
            .lock();
        let round = self
            .graph
            .run_publish_round_for_signed_dependency_outputs_of_parent_builds_in_recent_evaluations_until_deadline(
                start,
                gradient_db::caches::demotion::MAX_PUBLISHED_PARENT_BUILDS_PER_SIGNING_PASS,
                Instant::now() + DEPENDENCY_OUTPUT_PUBLISH_ROUND_DEADLINE,
            )
            .await;
        *self
            .last_signing_pass_handled_parent_build_derivation_id
            .lock() = round.last_handled_parent_build_id;
        round.failure.map_or(Ok(()), Err)
    }

    async fn repair_orphan_claims(&self) -> anyhow::Result<()> {
        let res = self
            .db
            .execute_raw(
                REPAIR_ORPHAN_CLAIMS.bind([sea_orm::Value::BigInt(Some(SIGN_SWEEP_BATCH as i64))]),
            )
            .await?;
        if res.rows_affected() > 0 {
            tracing::info!(
                count = res.rows_affected(),
                "sign sweep: re-created cache claims for paths that had none"
            );
        }
        Ok(())
    }

    async fn sign(&self) -> anyhow::Result<()> {
        self.sign_pending_signature_batch(SIGN_SWEEP_BATCH).await
    }

    async fn sign_pending_signature_batch(&self, batch_size: u64) -> anyhow::Result<()> {
        if let Err(e) = self.repair_orphan_claims().await {
            warn!(error = %e, "sign sweep: orphan claim repair failed");
        }

        let pending = ECachedPathSignature::find()
            .from_raw_sql(
                PENDING_SIGNATURES_THE_CACHE_MAY_SIGN
                    .bind([sea_orm::Value::BigInt(Some(batch_size as i64))]),
            )
            .all(&self.db)
            .await?;

        if pending.is_empty() {
            return Ok(());
        }
        let batch_was_full = pending.len() as u64 == batch_size;

        let signed = self
            .sign_pending_signature_rows_without_lock(pending)
            .await?;
        let stored = match self.store_signatures_if_signing_key_unchanged(signed).await {
            Ok(stored) => stored,
            Err(e) => {
                warn!(error = %e, "sign sweep: failed to persist signature");
                0
            }
        };
        if stored > 0 {
            tracing::info!(count = stored, "sign sweep: signatures filled");
            if batch_was_full {
                self.wake.notify_one();
            }
        }

        Ok(())
    }

    async fn sign_pending_signature_rows_without_lock(
        &self,
        pending: Vec<MCachedPathSignature>,
    ) -> anyhow::Result<Vec<SignedPathSignatureRowWithCachePrivateKey>> {
        let cache_ids: Vec<CacheId> = pending
            .iter()
            .map(|r| r.cache)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let cached_path_ids: Vec<CachedPathId> = pending
            .iter()
            .map(|r| r.cached_path)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let caches: HashMap<CacheId, MCache> = by_ids::<ECache, _>(&self.db, &cache_ids).await?;

        let cached_paths: HashMap<CachedPathId, MCachedPath> =
            by_ids::<ECachedPath, _>(&self.db, &cached_path_ids).await?;

        let mut signers: HashMap<CacheId, Option<CacheSigner>> = HashMap::new();
        for (cache_id, cache) in &caches {
            if cache.private_key.is_empty() {
                signers.insert(*cache_id, None);
                continue;
            }
            let signer = match CacheSigner::from_cache(
                &self.config.secrets.crypt_file,
                cache,
                &self.config.server.serve_url,
            ) {
                Ok(s) => Some(s),
                Err(e) => {
                    warn!(cache_name = %cache.name, error = %e, "sign sweep: failed to prepare signer");
                    None
                }
            };
            signers.insert(*cache_id, signer);
        }

        let mut signed = Vec::new();

        for row in pending {
            let Some(cache) = caches.get(&row.cache) else {
                continue;
            };
            let Some(Some(signer)) = signers.get(&row.cache) else {
                continue;
            };

            let Some(cp) = cached_paths.get(&row.cached_path) else {
                continue;
            };
            let store_path = cp.store_path();

            let refs: Vec<String> = cp
                .references
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_owned)
                .collect();

            let nar_hash_nix32 = normalize_nar_hash(&cp.nar_hash);

            let mapper = StorePathMapper::for_cache(Arc::clone(&self.store_path_mapping), cache.id);
            let sig_bytes = signer.sign_stored_narinfo(
                &mapper,
                &store_path,
                &nar_hash_nix32,
                cp.nar_size as u64,
                &refs,
            );

            debug!(cache_name = %cache.name, store_path = %store_path, "sign sweep: signed");
            signed.push(SignedPathSignatureRowWithCachePrivateKey {
                row,
                signature: sig_bytes,
                private_signing_key: cache.private_key.clone(),
            });
        }

        Ok(signed)
    }

    async fn store_signatures_if_signing_key_unchanged(
        &self,
        signed: Vec<SignedPathSignatureRowWithCachePrivateKey>,
    ) -> anyhow::Result<usize> {
        let cache_ids: HashSet<CacheId> = signed.iter().map(|signed| signed.row.cache).collect();

        let transaction = self.db.begin().await?;
        let current_private_keys: HashMap<CacheId, String> =
            gradient_db::caches::share_lock_caches_in_ascending_id_order_skipping_rows_locked_by_key_rotation_settings_edit_or_delete(
                &transaction,
                cache_ids.iter().copied().collect(),
            )
            .await?
            .into_iter()
            .map(|cache| (cache.id, cache.private_key))
            .collect();
        if current_private_keys.len() < cache_ids.len() {
            debug!(
                caches = cache_ids.len() - current_private_keys.len(),
                "sign sweep: key rotation took the lock on the cache row, so pending signatures for that cache wait for the next sweep"
            );
        }

        let mut stored = 0usize;
        for SignedPathSignatureRowWithCachePrivateKey {
            row,
            signature,
            private_signing_key,
        } in signed
        {
            if current_private_keys.get(&row.cache) != Some(&private_signing_key) {
                continue;
            }
            ECachedPathSignature::update_many()
                .col_expr(CCachedPathSignature::Signature, Expr::value(signature))
                .filter(CCachedPathSignature::Id.eq(row.id))
                .exec(&transaction)
                .await?;
            stored += 1;
        }
        transaction.commit().await?;
        Ok(stored)
    }
}

struct SignedPathSignatureRowWithCachePrivateKey {
    row: MCachedPathSignature,
    signature: Vec<u8>,
    private_signing_key: String,
}

#[cfg(test)]
mod sign_sweep_pending_signature_query_tests {
    use super::PENDING_SIGNATURES_THE_CACHE_MAY_SIGN;
    use gradient_fixtures::shape::shape;
    use gradient_types::*;
    use sea_orm::EntityTrait;

    #[tokio::test]
    async fn sign_sweep_resigns_previous_key_signed_cache_backfill_row_after_key_rotation_even_with_only_read_only_project_subscriber()
     {
        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path("backfilledhash", "backfilled")
            .into_cache(cache)
            .keyed("key")
            .with_legacy_cached_path_before_producer_tracking()
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE project_cache SET mode = 1, sign_outputs = false; \
             UPDATE cached_path_signature \
                 SET signature = NULL, previous_signing_key_signature = decode('01', 'hex') \
             WHERE cache = '{cache}';"
        ))
        .await;

        let pending = ECachedPathSignature::find()
            .from_raw_sql(
                PENDING_SIGNATURES_THE_CACHE_MAY_SIGN.bind([sea_orm::Value::BigInt(Some(10))]),
            )
            .all(&db.connection())
            .await
            .expect("pending signatures are readable");

        assert_eq!(
            pending.iter().map(|row| row.cache).collect::<Vec<_>>(),
            vec![cache],
            "Sweeps select backfilled content as the target for signing with the new key."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn queued_build_without_blocking_dependency_stays_queued_after_sign_sweep_signs_pending_signature()
     {
        use gradient_entity::build::BuildStatus;
        use gradient_entity::evaluation::EvaluationStatus;
        use gradient_storage::NarStore;
        use std::io::Write as _;

        const WAITING_BUILD_DERIVATION_STORE_PATH: &str = "cccccccccccccccccccccccccccccccc-c.drv";
        let mut secret = tempfile::NamedTempFile::new().expect("temp secret");
        secret
            .write_all(b"test-secret-key-32-bytes-padding!")
            .expect("write secret");
        let secret_path = secret.path().to_string_lossy().to_string();
        let private_key = gradient_sources::generate_signing_key(&secret_path)
            .expect("signing key")
            .0;
        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .evaluation_status(EvaluationStatus::Building)
            .cached_path("backfilledhash", "backfilled")
            .derivation(WAITING_BUILD_DERIVATION_STORE_PATH)
            .walked()
            .probed()
            .drv_cached()
            .entry_point("c")
            .job()
            .output("c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2")
            .build_status(BuildStatus::Queued)
            .into_cache(cache)
            .keyed(private_key)
            .with_legacy_cached_path_before_producer_tracking()
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE project_cache SET mode = 1, sign_outputs = false; \
             UPDATE cached_path_signature \
                 SET signature = NULL, previous_signing_key_signature = decode('01', 'hex') \
             WHERE cache = '{cache}';"
        ))
        .await;
        let status_and_blocking_count = async || {
            let build = db.shared_build(WAITING_BUILD_DERIVATION_STORE_PATH).await;
            (build.status, build.blocking_deps)
        };
        assert_eq!(status_and_blocking_count().await, (BuildStatus::Queued, 0));
        let nar_dir = tempfile::tempdir().expect("tempdir");
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(nar_dir.path()).expect("store"),
            db.connection(),
            move |config| config.secrets.crypt_file = secret_path,
        )
        .await;
        let sweep =
            build_sign_sweep_from_test_server_state(&state, std::time::Duration::from_secs(60));

        sweep.sign().await.expect("the sign sweep runs");

        assert!(
            ECachedPathSignature::find()
                .all(&db.connection())
                .await
                .expect("signature read")
                .iter()
                .all(|row| row.signature.is_some())
        );
        assert_eq!(status_and_blocking_count().await, (BuildStatus::Queued, 0));
        db.finish().await;
    }

    #[tokio::test]
    async fn key_rotation_between_signing_and_storing_leaves_no_signature_of_retired_key() {
        use gradient_storage::NarStore;

        let (_secret, secret_path, private_key) =
            write_new_cache_signing_key_to_temporary_secret_file();
        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path("backfilledhash", "backfilled")
            .into_cache(cache)
            .keyed(private_key)
            .with_legacy_cached_path_before_producer_tracking()
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE project_cache SET mode = 1, sign_outputs = false; \
             UPDATE cached_path_signature \
                 SET signature = NULL, previous_signing_key_signature = decode('01', 'hex') \
             WHERE cache = '{cache}';"
        ))
        .await;
        let nar_dir = tempfile::tempdir().expect("tempdir");
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(nar_dir.path()).expect("store"),
            db.connection(),
            move |config| config.secrets.crypt_file = secret_path,
        )
        .await;
        let sweep =
            build_sign_sweep_from_test_server_state(&state, std::time::Duration::from_secs(60));
        let conn = db.connection();
        let pending = ECachedPathSignature::find()
            .from_raw_sql(
                PENDING_SIGNATURES_THE_CACHE_MAY_SIGN.bind([sea_orm::Value::BigInt(Some(10))]),
            )
            .all(&conn)
            .await
            .expect("pending signatures are readable");
        let signed = sweep
            .sign_pending_signature_rows_without_lock(pending)
            .await
            .expect("pending rows signed");
        assert!(
            !signed.is_empty(),
            "The sign sweep signs the pending row of cache C before the key rotation."
        );
        let cache_row = ECache::find_by_id(cache)
            .one(&conn)
            .await
            .expect("Read the cache.")
            .expect("Read the cache.");
        gradient_db::caches::insert_old_cache_key_row_replace_key_pair_and_move_signatures_to_previous_column(
            &conn,
            cache_row,
            "retired-public-key".to_owned(),
            ("new-private-key".to_owned(), "new-public-key".to_owned()),
        )
        .await
        .expect("Rotate the cache signing key.");

        sweep
            .store_signatures_if_signing_key_unchanged(signed)
            .await
            .expect("save transaction to commit");

        let signatures: Vec<Option<Vec<u8>>> = ECachedPathSignature::find()
            .all(&conn)
            .await
            .expect("signature read")
            .into_iter()
            .filter(|row| row.cache == cache)
            .map(|row| row.signature)
            .collect();
        assert!(
            signatures.iter().all(Option::is_none),
            "A key rotation of cache C committed after the signature sweep signed a batch and before the sweep saved that batch. No signatures of the retired key remain in cache C: {signatures:?}"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn sign_sweep_store_step_does_not_wait_on_open_key_rotation_and_next_sweep_signs_with_new_key()
     {
        use gradient_storage::NarStore;
        use sea_orm::TransactionTrait;

        let (_secret, secret_path, private_key) =
            write_new_cache_signing_key_to_temporary_secret_file();
        let (new_private_key, new_public_key) =
            gradient_sources::generate_signing_key(&secret_path).expect("new signing key");
        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path("backfilledhash", "backfilled")
            .into_cache(cache)
            .keyed(private_key)
            .with_legacy_cached_path_before_producer_tracking()
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE project_cache SET mode = 1, sign_outputs = false; \
             UPDATE cached_path_signature \
                 SET signature = NULL, previous_signing_key_signature = decode('01', 'hex') \
             WHERE cache = '{cache}';"
        ))
        .await;
        let nar_dir = tempfile::tempdir().expect("tempdir");
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(nar_dir.path()).expect("store"),
            db.connection(),
            move |config| config.secrets.crypt_file = secret_path,
        )
        .await;
        let sweep =
            build_sign_sweep_from_test_server_state(&state, std::time::Duration::from_secs(60));
        let conn = db.connection();
        let signatures_in_cache = || async {
            ECachedPathSignature::find()
                .all(&conn)
                .await
                .expect("signature read")
                .into_iter()
                .filter(|row| row.cache == cache)
                .map(|row| row.signature)
                .collect::<Vec<Option<Vec<u8>>>>()
        };
        let pending = ECachedPathSignature::find()
            .from_raw_sql(
                PENDING_SIGNATURES_THE_CACHE_MAY_SIGN.bind([sea_orm::Value::BigInt(Some(10))]),
            )
            .all(&conn)
            .await
            .expect("pending signatures are readable");
        let signed = sweep
            .sign_pending_signature_rows_without_lock(pending)
            .await
            .expect("pending rows signed");
        let rotation = conn.begin().await.expect("begin");
        gradient_db::caches::insert_old_cache_key_row_replace_key_pair_and_move_signatures_to_previous_column(
            &rotation,
            ECache::find_by_id(cache)
                .one(&conn)
                .await
                .expect("Read the cache.")
                .expect("Read the cache."),
            "retired-public-key".to_owned(),
            (new_private_key, new_public_key),
        )
        .await
        .expect("rotation to update cache row in open transaction");

        let stored = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            sweep.store_signatures_if_signing_key_unchanged(signed),
        )
        .await;
        assert!(
            matches!(stored, Ok(Ok(0))),
            "The sign sweep stores nothing and returns without waiting while a key rotation of cache C is open: {stored:?}"
        );
        assert!(
            signatures_in_cache().await.iter().all(Option::is_none),
            "Signature rows of cache C stay unsigned while the key rotation is open."
        );
        rotation.commit().await.expect("commit rotation");

        sweep.sign().await.expect("next sweep run");
        let rotated_cache = ECache::find_by_id(cache)
            .one(&conn)
            .await
            .expect("Read the cache.")
            .expect("Read the cache.");
        let serve_url = state.config.server.serve_url.clone();
        let new_public_key = gradient_sources::format_cache_public_key(
            &state.config.secrets.crypt_file,
            MCache {
                public_key: String::new(),
                ..rotated_cache.clone()
            },
            serve_url.clone(),
        )
        .expect("public key derived from the new private signing key of cache C");
        let cached_path = ECachedPath::find()
            .one(&conn)
            .await
            .expect("Read the cached path.")
            .expect("Read the cached path.");
        let signatures = signatures_in_cache().await;
        let verifies_with_new_key = |signature: &Vec<u8>| {
            gradient_sources::verify_narinfo_signature(
                &new_public_key,
                &NixPathInfo {
                    store_path: cached_path.store_path(),
                    url: String::new(),
                    compression: String::new(),
                    file_hash: None,
                    file_size: None,
                    nar_hash: gradient_util::nix_hash::normalize_nar_hash(&cached_path.nar_hash),
                    nar_size: cached_path.nar_size as u64,
                    references: cached_path
                        .references
                        .as_deref()
                        .unwrap_or_default()
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect(),
                    signature_lines: vec![gradient_sources::full_signature_token(
                        signature,
                        &serve_url,
                        &rotated_cache.name,
                    )],
                    deriver: None,
                    ca: None,
                },
            )
        };
        assert!(
            !signatures.is_empty()
                && signatures
                    .iter()
                    .all(|signature| signature.as_ref().is_some_and(verifies_with_new_key)),
            "The single next sweep run after the commit of the key rotation must sign the row of cache C with a signature that the new public key of cache C can verify: {signatures:?}"
        );
        db.finish().await;
    }

    fn build_sign_sweep_from_test_server_state(
        state: &std::sync::Arc<gradient_core::ServerState>,
        period: std::time::Duration,
    ) -> super::SignSweep {
        use std::sync::Arc;

        super::SignSweep {
            db: state.worker_db.clone(),
            graph: Arc::clone(&state.graph),
            config: Arc::clone(&state.config),
            store_path_mapping: Arc::clone(&state.extensions.store_path_mapping),
            period,
            wake: Arc::new(tokio::sync::Notify::new()),
            last_signing_pass_handled_parent_build_derivation_id: Default::default(),
            last_signing_started_at: Default::default(),
            last_complete_sweep_finished_at: Default::default(),
            publish_start_and_scheduled_delayed_wake:
                super::SignatureSweepPublishStartAndPendingWake::new(state.shutdown.clone()),
        }
    }

    const PARENT_BUILD_DERIVATION_STORE_PATH: &str = "11111111111111111111111111111111-parent.drv";
    const PARENT_BUILD_OUTPUT_STORE_PATH_HASH: &str = "33333333333333333333333333333333";
    const DEPENDENCY_BUILD_DERIVATION_STORE_PATH: &str =
        "22222222222222222222222222222222-dependency.drv";
    const DEPENDENCY_OUTPUT_STORE_PATH_HASH: &str = "44444444444444444444444444444444";
    const OTHER_CACHE_UNSIGNED_PLACEHOLDER_STORE_PATH_HASH: &str =
        "66666666666666666666666666666666";

    fn write_new_cache_signing_key_to_temporary_secret_file()
    -> (tempfile::NamedTempFile, String, String) {
        use std::io::Write as _;

        let mut secret = tempfile::NamedTempFile::new().expect("temp secret");
        secret
            .write_all(b"test-secret-key-32-bytes-padding!")
            .expect("write secret");
        let secret_path = secret.path().to_string_lossy().to_string();
        let private_key = gradient_sources::generate_signing_key(&secret_path)
            .expect("signing key")
            .0;
        (secret, secret_path, private_key)
    }

    const FORBIDDEN_OUTPUT_DERIVATION_STORE_PATH: &str =
        "56565656565656565656565656565656-forbidden.drv";
    const FORBIDDEN_OUTPUT_STORE_PATH_HASH: &str = "77777777777777777777777777777777";
    const UNKEYED_CACHE_STORE_PATH_HASH: &str = "88888888888888888888888888888888";
    const SIGNABLE_STORE_PATH_HASHES: [&str; 2] = [
        "99999999999999999999999999999999",
        "12121212121212121212121212121212",
    ];

    async fn database_with_forbidden_unkeyed_and_signable_pending_signatures(
        signable_rows: usize,
    ) -> (
        gradient_fixtures::database::TestDatabase,
        super::SignSweep,
        impl Sized,
    ) {
        use gradient_entity::build::BuildStatus;
        use gradient_storage::NarStore;

        let (secret, secret_path, private_key) =
            write_new_cache_signing_key_to_temporary_secret_file();
        let (keyed_cache, unkeyed_cache) = (CacheId::now_v7(), CacheId::now_v7());
        let mut seed = shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path(FORBIDDEN_OUTPUT_STORE_PATH_HASH, "forbidden")
            .into_cache(keyed_cache)
            .keyed(private_key.clone())
            .with_legacy_cached_path_before_producer_tracking()
            .derivation(FORBIDDEN_OUTPUT_DERIVATION_STORE_PATH)
            .walked()
            .probed()
            .drv_cached()
            .entry_point("forbidden")
            .job()
            .output(FORBIDDEN_OUTPUT_STORE_PATH_HASH)
            .build_status(BuildStatus::Completed)
            .evaluation(EvaluationId::now_v7())
            .cached_path(UNKEYED_CACHE_STORE_PATH_HASH, "unkeyed")
            .into_cache(unkeyed_cache)
            .with_legacy_cached_path_before_producer_tracking();
        for hash in &SIGNABLE_STORE_PATH_HASHES[..signable_rows] {
            seed = seed
                .evaluation(EvaluationId::now_v7())
                .cached_path(hash, "signable")
                .into_cache(keyed_cache)
                .keyed(private_key.clone())
                .with_legacy_cached_path_before_producer_tracking();
        }
        let db = seed.into_database().await;
        db.execute(
            "UPDATE project_cache SET mode = 1, sign_outputs = false; \
             UPDATE cached_path_signature \
                 SET signature = NULL, previous_signing_key_signature = decode('01', 'hex');",
        )
        .await;
        let nar_dir = tempfile::tempdir().expect("tempdir");
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(nar_dir.path()).expect("store"),
            db.connection(),
            move |config| config.secrets.crypt_file = secret_path,
        )
        .await;
        let sweep =
            build_sign_sweep_from_test_server_state(&state, std::time::Duration::from_secs(60));
        (db, sweep, (secret, nar_dir, state))
    }

    fn sweep_wake_has_stored_notification(sweep: &super::SignSweep) -> bool {
        tokio_test::task::spawn(sweep.wake.notified())
            .poll()
            .is_ready()
    }

    #[tokio::test]
    async fn signature_sweep_batch_with_no_rows_it_can_sign_does_not_wake_again() {
        let (db, sweep, _guards) =
            database_with_forbidden_unkeyed_and_signable_pending_signatures(0).await;

        sweep
            .sign_pending_signature_batch(2)
            .await
            .expect("the sign sweep runs");

        assert!(
            !sweep_wake_has_stored_notification(&sweep),
            "A batch with only rows the sweep cannot sign can end its run without a new sweep run."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn sweep_signs_pending_signable_row_after_unsignable_rows_without_external_wake() {
        let (db, sweep, _guards) =
            database_with_forbidden_unkeyed_and_signable_pending_signatures(1).await;

        sweep
            .sign_pending_signature_batch(2)
            .await
            .expect("the sign sweep runs");
        let mut runs_after_own_wake = 0;
        while runs_after_own_wake < 3 && sweep_wake_has_stored_notification(&sweep) {
            sweep
                .sign_pending_signature_batch(2)
                .await
                .expect("the sign sweep runs");
            runs_after_own_wake += 1;
        }

        let signatures = db.signatures(SIGNABLE_STORE_PATH_HASHES[0]).await;
        assert!(
            !signatures.is_empty() && signatures.iter().all(|row| row.signature.is_some()),
            "A sweep can sign signable rows after rows that can't be signed. No wake can come from outside the sweep."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn full_batch_of_pending_signature_rows_stores_signatures_and_wakes_sweep_immediately() {
        let (db, sweep, _guards) =
            database_with_forbidden_unkeyed_and_signable_pending_signatures(2).await;

        sweep
            .sign_pending_signature_batch(1)
            .await
            .expect("the sign sweep runs");

        let mut signed_rows = 0;
        for hash in SIGNABLE_STORE_PATH_HASHES {
            signed_rows += db
                .signatures(hash)
                .await
                .iter()
                .filter(|row| row.signature.is_some())
                .count();
        }
        assert_eq!(
            (signed_rows, sweep_wake_has_stored_notification(&sweep)),
            (1, true),
            "Full batches of signable rows save the signatures and make the sweep rerun right away."
        );
        db.finish().await;
    }

    const PREVIOUS_PENDING_SIGNATURE_SQL_TEMPLATE_WITHOUT_OFFSET: &str = "SELECT s.* FROM cached_path_signature s \
         WHERE s.signature IS NULL AND {accepted} \
           AND EXISTS (SELECT 1 FROM cache keyed_cache \
                       WHERE keyed_cache.id = s.cache AND keyed_cache.private_key <> '') \
           AND NOT EXISTS (SELECT 1 FROM cached_path pending_path \
                           JOIN derivation_output pending_output ON pending_output.hash = pending_path.hash \
                           WHERE pending_path.id = s.cached_path AND {must_not_be_signed}) \
         ORDER BY s.id \
         LIMIT $1";

    async fn run_pending_signature_query_with_limit_and_get_cached_path_signature_ids(
        db: &gradient_fixtures::database::TestDatabase,
        sql: String,
        limit: i64,
    ) -> Vec<CachedPathSignatureId> {
        ECachedPathSignature::find()
            .from_raw_sql(
                PENDING_SIGNATURES_THE_CACHE_MAY_SIGN
                    .bind_built(sql, [sea_orm::Value::BigInt(Some(limit))]),
            )
            .all(&db.connection())
            .await
            .expect("pending signatures are readable")
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    #[tokio::test]
    async fn pending_signature_query_without_offset_returns_same_rows_in_same_order_as_with_offset()
    {
        let previous_text = PREVIOUS_PENDING_SIGNATURE_SQL_TEMPLATE_WITHOUT_OFFSET
            .replace(
                "{accepted}",
                &super::cache_can_sign_content_variant_sql_condition("s.cached_path", "s.cache"),
            )
            .replace(
                "{must_not_be_signed}",
                &super::sql_condition_for_output_that_must_not_be_signed("pending_output"),
            );
        let states = [
            (
                "Private output and cache without key, no signable row",
                0,
                false,
            ),
            (
                "Two signable rows after the private output and the keyless cache",
                2,
                false,
            ),
            (
                "One signable row already signed. Another row still pending.",
                2,
                true,
            ),
        ];
        for (state, signable_rows, first_signable_row_signed) in states {
            let (db, _sweep, _guards) =
                database_with_forbidden_unkeyed_and_signable_pending_signatures(signable_rows)
                    .await;
            if first_signable_row_signed {
                db.execute(&format!(
                    "UPDATE cached_path_signature SET signature = decode('02', 'hex') \
                     WHERE cached_path IN (SELECT id FROM cached_path WHERE hash = '{}');",
                    SIGNABLE_STORE_PATH_HASHES[0]
                ))
                .await;
            }
            for limit in [1, 2, 3, 10] {
                let current =
                    run_pending_signature_query_with_limit_and_get_cached_path_signature_ids(
                        &db,
                        PENDING_SIGNATURES_THE_CACHE_MAY_SIGN.text().into_owned(),
                        limit,
                    )
                    .await;
                assert_eq!(
                    run_pending_signature_query_with_limit_and_get_cached_path_signature_ids(
                        &db,
                        previous_text.clone(),
                        limit,
                    )
                    .await,
                    current,
                    "The previous and current queries for pending signatures return the same rows in the same order. Both use state {state} and limit {limit}."
                );
                if signable_rows > 0 && limit == 1 {
                    assert_eq!(
                        current.len(),
                        1,
                        "The current query for pending signatures with LIMIT 1 can return a signable row after non-signable rows."
                    );
                }
            }
            db.finish().await;
        }
    }

    fn add_queued_project_p_build_waiting_on_project_q_stored_output(
        seed: gradient_fixtures::shape::Shape,
        signing_cache: CacheId,
        private_key: &str,
    ) -> gradient_fixtures::shape::Shape {
        use gradient_entity::build::BuildStatus;
        use gradient_entity::evaluation::EvaluationStatus;

        seed.evaluation(EvaluationId::now_v7())
            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")
            .evaluation(EvaluationId::now_v7())
            .evaluation_status(EvaluationStatus::Building)
            .into_cache(signing_cache)
            .keyed(private_key)
            .derivation(PARENT_BUILD_DERIVATION_STORE_PATH)
            .walked()
            .probed()
            .drv_cached()
            .entry_point("parent")
            .job()
            .output(PARENT_BUILD_OUTPUT_STORE_PATH_HASH)
            .depends_on(DEPENDENCY_BUILD_DERIVATION_STORE_PATH)
            .build_status(BuildStatus::Queued)
            .derivation(DEPENDENCY_BUILD_DERIVATION_STORE_PATH)
            .walked()
            .probed()
            .drv_cached()
            .job()
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .build_status(BuildStatus::Completed)
    }

    #[tokio::test]
    async fn sign_sweep_signs_dependency_output_with_consistency_check_off() {
        use gradient_storage::NarStore;
        use gradient_util::supervision::Pass;
        use std::time::Duration;

        let (_secret, secret_path, private_key) =
            write_new_cache_signing_key_to_temporary_secret_file();
        let signing_cache = CacheId::now_v7();
        let db = add_queued_project_p_build_waiting_on_project_q_stored_output(
            shape(),
            signing_cache,
            &private_key,
        )
        .into_database()
        .await;
        let nar_dir = tempfile::tempdir().expect("tempdir");
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(nar_dir.path()).expect("store"),
            db.connection(),
            move |config| {
                config.secrets.crypt_file = secret_path;
                config.metrics_args.graph_consistency_interval_secs = 0;
            },
        )
        .await;
        state
            .graph
            .run_consistency_checks()
            .await
            .expect("the seed settles");
        let sweep = build_sign_sweep_from_test_server_state(&state, Duration::from_secs(3600));
        assert_eq!(sweep.period(), Duration::from_secs(300));

        sweep.run().await.expect("the sign sweep runs");

        assert!(
            db.signatures(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
                .await
                .iter()
                .any(|row| row.cache == signing_cache && row.signature.is_some()),
            "One sign sweep run publishes the signed dependency output of project Q into cache S of project P."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn publishing_failure_for_one_parent_build_sets_it_aside_and_signs_placeholder_in_other_cache()
     {
        use gradient_storage::NarStore;
        use gradient_util::supervision::Pass;

        let (_secret, secret_path, private_key) =
            write_new_cache_signing_key_to_temporary_secret_file();
        let (signing_cache, placeholder_cache) = (CacheId::now_v7(), CacheId::now_v7());
        let seed = shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path(
                OTHER_CACHE_UNSIGNED_PLACEHOLDER_STORE_PATH_HASH,
                "placeholder",
            )
            .into_cache(placeholder_cache)
            .keyed(private_key.clone())
            .with_legacy_cached_path_before_producer_tracking();
        let db = add_queued_project_p_build_waiting_on_project_q_stored_output(
            seed,
            signing_cache,
            &private_key,
        )
        .into_database()
        .await;
        db.execute(&format!(
            "UPDATE project_cache SET mode = 1, sign_outputs = false \
             WHERE cache = '{placeholder_cache}'; \
             UPDATE cached_path_signature \
                 SET signature = NULL, previous_signing_key_signature = decode('01', 'hex') \
             WHERE cache = '{placeholder_cache}'; \
             CREATE FUNCTION reject_signing_cache_signature_insert() \
             RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
               RAISE EXCEPTION 'injected'; \
             END $$; \
             CREATE TRIGGER reject_signing_cache_signature_insert \
               BEFORE INSERT ON cached_path_signature FOR EACH ROW \
               WHEN (NEW.cache = '{signing_cache}') \
               EXECUTE FUNCTION reject_signing_cache_signature_insert();"
        ))
        .await;
        let nar_dir = tempfile::tempdir().expect("tempdir");
        let state = crate::test_support::build_test_server_state_with_running_graph_writer(
            NarStore::local(nar_dir.path()).expect("store"),
            db.connection(),
            move |config| config.secrets.crypt_file = secret_path,
        )
        .await;
        state
            .graph
            .run_consistency_checks()
            .await
            .expect("the seed settles");
        let sweep =
            build_sign_sweep_from_test_server_state(&state, std::time::Duration::from_secs(60));
        let publish_cursor_before_sign_sweep = Some(DerivationId::nil());
        *sweep
            .last_signing_pass_handled_parent_build_derivation_id
            .lock() = publish_cursor_before_sign_sweep;

        let run = sweep.run().await;

        let cursor_after_run = *sweep
            .last_signing_pass_handled_parent_build_derivation_id
            .lock();
        assert_eq!(
            (
                run.is_ok(),
                cursor_after_run
                    .is_some_and(|cursor| publish_cursor_before_sign_sweep != Some(cursor))
            ),
            (true, true),
            "Failures to publish to cache S set aside that single parent build, move the cursor past it and don't count as an error for that run."
        );
        assert!(
            db.signatures(OTHER_CACHE_UNSIGNED_PLACEHOLDER_STORE_PATH_HASH)
                .await
                .iter()
                .any(|row| row.cache == placeholder_cache && row.signature.is_some()),
            "The run signed the placeholder in the other cache after publish failed."
        );
        db.finish().await;
    }

    const OTHER_PROJECT_WORKER_BUILT_STORE_PATH_HASH: &str = "55555555555555555555555555555555";

    async fn build_database_with_worker_path_and_read_write_signing_subscriptions_to_two_keyed_caches()
    -> (gradient_fixtures::database::TestDatabase, CacheId, CacheId) {
        let cache_id_for_server_signed_row_with_rotated_key = CacheId::now_v7();
        let signed_variant_second_cache_id = CacheId::now_v7();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path(OTHER_PROJECT_WORKER_BUILT_STORE_PATH_HASH, "built")
            .mark_cached_path_built_by_team_worker()
            .evaluation(EvaluationId::now_v7())
            .into_cache(cache_id_for_server_signed_row_with_rotated_key)
            .keyed("key")
            .into_cache(signed_variant_second_cache_id)
            .keyed("key")
            .into_database()
            .await;
        (
            db,
            cache_id_for_server_signed_row_with_rotated_key,
            signed_variant_second_cache_id,
        )
    }

    #[tokio::test]
    async fn sign_sweep_after_key_rotation_selects_other_project_worker_variant_server_signed_row_only_for_read_write_cache_signing_subscriber_and_not_for_read_only_write_only_inactive_or_placeholder_only()
     {
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use sea_orm::ActiveEnum;

        let (db, cache_id_for_server_signed_row_with_rotated_key, signed_variant_second_cache_id) =
            build_database_with_worker_path_and_read_write_signing_subscriptions_to_two_keyed_caches()
                .await;
        db.execute(&format!(
            "INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             SELECT uuidv7(), id, '{cache_id_for_server_signed_row_with_rotated_key}', decode('01', 'hex'), 0, now() \
             FROM cached_path WHERE hash = '{OTHER_PROJECT_WORKER_BUILT_STORE_PATH_HASH}'"
        ))
        .await;
        let conn = db.connection();
        let cache = ECache::find_by_id(cache_id_for_server_signed_row_with_rotated_key)
            .one(&conn)
            .await
            .expect("Read the cache.")
            .expect("Read the cache.");
        gradient_db::caches::insert_old_cache_key_row_replace_key_pair_and_move_signatures_to_previous_column(
            &conn,
            cache,
            "retired-public-key".to_owned(),
            ("new-private-key".to_owned(), "new-public-key".to_owned()),
        )
        .await
        .expect("Rotate the cache signing key.");
        let cache_ids_sign_sweep_would_sign_now = async || {
            ECachedPathSignature::find()
                .from_raw_sql(
                    PENDING_SIGNATURES_THE_CACHE_MAY_SIGN.bind([sea_orm::Value::BigInt(Some(10))]),
                )
                .all(&conn)
                .await
                .expect("pending signatures are readable")
                .into_iter()
                .map(|row| row.cache)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            cache_ids_sign_sweep_would_sign_now().await,
            vec![cache_id_for_server_signed_row_with_rotated_key],
            "Key rotations make the sweep sign the rows that the server signed again."
        );

        let condition_remove_and_restore_sql_statement_pairs = [
            (
                format!(
                    "UPDATE project_cache SET mode = {}, sign_outputs = false",
                    CacheSubscriptionMode::ReadOnly.to_value()
                ),
                format!(
                    "UPDATE project_cache SET mode = {}, sign_outputs = true",
                    CacheSubscriptionMode::ReadWrite.to_value()
                ),
            ),
            (
                format!(
                    "UPDATE project_cache SET mode = {}",
                    CacheSubscriptionMode::WriteOnly.to_value()
                ),
                format!(
                    "UPDATE project_cache SET mode = {}",
                    CacheSubscriptionMode::ReadWrite.to_value()
                ),
            ),
            (
                format!(
                    "UPDATE cache SET active = false WHERE id = '{cache_id_for_server_signed_row_with_rotated_key}'"
                ),
                format!(
                    "UPDATE cache SET active = true WHERE id = '{cache_id_for_server_signed_row_with_rotated_key}'"
                ),
            ),
            (
                format!(
                    "UPDATE cached_path_signature SET previous_signing_key_signature = NULL; \
                     INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
                     SELECT uuidv7(), id, '{signed_variant_second_cache_id}', decode('01', 'hex'), 0, now() \
                     FROM cached_path WHERE hash = '{OTHER_PROJECT_WORKER_BUILT_STORE_PATH_HASH}'"
                ),
                String::new(),
            ),
        ];
        let mut selected_after_removal = Vec::new();
        for (
            trust_condition_removal_statement,
            sql_statement_that_restores_removed_trust_condition,
        ) in condition_remove_and_restore_sql_statement_pairs
        {
            db.execute(&trust_condition_removal_statement).await;
            selected_after_removal.push(cache_ids_sign_sweep_would_sign_now().await);
            if !sql_statement_that_restores_removed_trust_condition.is_empty() {
                db.execute(&sql_statement_that_restores_removed_trust_condition)
                    .await;
            }
        }
        assert_eq!(
            selected_after_removal,
            vec![Vec::<CacheId>::new(); 4],
            "The sweep can skip rows for ReadOnly, WriteOnly, or inactive subscriptions. The sweep can also skip a row when another cache alone can store the signature for that variant."
        );
        db.finish().await;
    }
}

#[cfg(test)]
mod orphan_claim_tests {
    use super::REPAIR_ORPHAN_CLAIMS;

    /// An unbounded fixpoint over `cached_path` starved this scheduler before. The pass is running
    /// on a timer.
    #[test]
    fn the_repair_is_bounded_and_respects_the_sign_outputs_opt_out() {
        let sql = REPAIR_ORPHAN_CLAIMS.text();
        assert!(sql.contains("LIMIT $1"), "{sql}");
        assert!(
            sql.contains("claim_subscription.sign_outputs"),
            "Pass no claims to subscriptions that opted out of signing."
        );
    }
}

#[cfg(test)]
mod sign_sweep_publish_step_gap_tests {
    use super::{
        SIGN_SWEEP_PUBLISH_MINIMUM_TIME_BETWEEN_STARTS, SignatureSweepPublishStartAndPendingWake,
    };
    use gradient_util::shutdown::Shutdown;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;
    use tokio::time::Instant;

    #[tokio::test(start_paused = true)]
    async fn second_wake_within_two_second_publish_gap_adds_exactly_one_publish_step() {
        let gate = SignatureSweepPublishStartAndPendingWake::new(Shutdown::new());
        let wake = Arc::new(Notify::new());
        let first_publish_started_at = Instant::now();
        let mut publish_step_starts = vec![first_publish_started_at];
        assert!(gate.try_start_publish_or_schedule_next_publish_wake(true, &wake));
        tokio::time::advance(Duration::from_millis(500)).await;
        assert!(!gate.try_start_publish_or_schedule_next_publish_wake(true, &wake));
        assert!(!gate.try_start_publish_or_schedule_next_publish_wake(true, &wake));

        while tokio::time::timeout(Duration::from_secs(30), wake.notified())
            .await
            .is_ok()
        {
            if gate.try_start_publish_or_schedule_next_publish_wake(true, &wake) {
                publish_step_starts.push(Instant::now());
            }
        }

        assert_eq!(
            (
                publish_step_starts.len(),
                publish_step_starts.last().is_some_and(|started| {
                    *started - first_publish_started_at
                        >= SIGN_SWEEP_PUBLISH_MINIMUM_TIME_BETWEEN_STARTS
                })
            ),
            (2, true),
            "Two wake events within 2 seconds cause two publish steps. The first and second publish steps start right away and after 2 seconds pass."
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_with_tiny_drain_budget_cancels_delayed_wake_armed_by_two_wakes_inside_publish_gap_and_no_publish_step_starts_after_gap()
     {
        let server_shutdown = Shutdown::new();
        let gate = SignatureSweepPublishStartAndPendingWake::new(server_shutdown.clone());
        let wake = Arc::new(Notify::new());
        assert!(gate.try_start_publish_or_schedule_next_publish_wake(true, &wake));
        tokio::time::advance(Duration::from_millis(500)).await;
        assert!(!gate.try_start_publish_or_schedule_next_publish_wake(true, &wake));
        assert!(!gate.try_start_publish_or_schedule_next_publish_wake(true, &wake));

        let drain_started_at = Instant::now();
        let drained = server_shutdown
            .cancel_and_drain(Duration::from_millis(10))
            .await;
        let cancel_and_drain_elapsed_on_paused_clock = drain_started_at.elapsed();

        tokio::time::advance(SIGN_SWEEP_PUBLISH_MINIMUM_TIME_BETWEEN_STARTS * 2).await;
        let mut publish_steps_started_after_shutdown = 0;
        while tokio::time::timeout(Duration::from_secs(30), wake.notified())
            .await
            .is_ok()
        {
            if gate.try_start_publish_or_schedule_next_publish_wake(true, &wake) {
                publish_steps_started_after_shutdown += 1;
            }
        }

        assert_eq!(
            (
                drained,
                cancel_and_drain_elapsed_on_paused_clock
                    < SIGN_SWEEP_PUBLISH_MINIMUM_TIME_BETWEEN_STARTS,
                publish_steps_started_after_shutdown
            ),
            (true, true, 0),
            "Server shutdowns drain immediately with a delayed wake scheduled, before the end of the 2 second gap. No publish step can start after shutdown."
        );
    }
}
