/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::store_path_content_variant_reader_choice::cached_path_by_store_hash_subquery_sql;
use crate::store_path_content_variant_trust::{
    cache_trusts_content_producer_sql, reading_subscription,
};
use chrono::NaiveDateTime;
pub use gradient_api_types::NarSummary;
use gradient_types::*;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, QueryFilter,
    Value,
};

pub async fn signature_in_cache<C: ConnectionTrait>(
    db: &C,
    cached_path: CachedPathId,
    cache: CacheId,
) -> Result<Option<MCachedPathSignature>, DbErr> {
    ECachedPathSignature::find()
        .filter(
            Condition::all()
                .add(CCachedPathSignature::CachedPath.eq(cached_path))
                .add(CCachedPathSignature::Cache.eq(cache)),
        )
        .one(db)
        .await
}

crate::sql_fn! {
    BUILD_ATTEMPT_THAT_PRODUCED_DERIVATION_OUTPUT_SERVED_BY_CACHE = || format!(
        "SELECT a.id AS id \
         FROM derivation d \
         JOIN derivation_output o ON o.derivation = d.id \
         CROSS JOIN LATERAL ({served}) served \
         JOIN store_path_content_producer p ON p.cached_path = served.id AND {trusted} \
         JOIN build_attempt a ON a.dispatched_job = p.dispatched_job \
         JOIN derivation_build b ON b.id = a.derivation_build AND b.derivation = d.id \
         WHERE d.hash = $1 AND d.name = $2 \
           AND EXISTS (SELECT 1 FROM build_job cache_reading_build_job \
                       JOIN evaluation ON evaluation.id = cache_reading_build_job.evaluation \
                       JOIN task eval_task ON eval_task.id = evaluation.task \
                       WHERE cache_reading_build_job.derivation_build = b.id \
                         AND (evaluation.finished_at IS NULL OR evaluation.finished_at >= a.created_at) \
                         AND EXISTS ({reads} AND trust_read.cache = $3::uuid)) \
         ORDER BY a.created_at DESC, a.id DESC \
         LIMIT 1",
        served = cached_path_by_store_hash_subquery_sql("o.hash", "$3::uuid"),
        trusted = cache_trusts_content_producer_sql("p", "$3::uuid"),
        reads = reading_subscription("eval_task.project"),
    ),
        params = [DerivationHash, Text("hello"), CacheId];
}

crate::sql! {
    TOUCH_CACHED_PATH_SIGNATURE_FETCH = "UPDATE cached_path_signature \
         SET last_fetched_at = $1, fetch_count = fetch_count + 1 \
         WHERE cache = $2 AND store_path_hash = $3",
        params = [Now, CacheId, CachedPathHash];

    NAR_STATS = "SELECT COUNT(*) AS total_nars, \
                COALESCE(SUM(cp.nar_size),0)::bigint AS total_nar_size, \
                COALESCE(SUM(cp.file_size),0)::bigint AS total_file_size, \
                MAX(cp.created_at) AS last_uploaded_at, \
                MIN(cps.last_fetched_at) AS oldest_fetched_at \
         FROM cached_path_signature cps \
         JOIN cached_path cp ON cp.id = cps.cached_path \
         WHERE cps.cache = $1",
        params = [CacheId],
        tier = Bulk;
}

pub async fn build_attempt_that_produced_derivation_output_served_by_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    drv_hash: &str,
    drv_name: &str,
) -> Result<Option<BuildAttemptId>, DbErr> {
    let row = db
        .query_one_raw(
            BUILD_ATTEMPT_THAT_PRODUCED_DERIVATION_OUTPUT_SERVED_BY_CACHE.bind([
                drv_hash.into(),
                drv_name.into(),
                cache.into_inner().into(),
            ]),
        )
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<uuid::Uuid>("", "id").ok())
        .map(BuildAttemptId::new))
}

pub async fn count_fetch<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    hash: &str,
) -> Result<(), DbErr> {
    let now = gradient_types::now();
    let now_val = Value::ChronoDateTimeUtc(Some(chrono::DateTime::from_naive_utc_and_offset(
        now,
        chrono::Utc,
    )));
    let cache_val = Value::Uuid(Some(cache.into_inner()));
    let hash_val = Value::String(Some(hash.to_owned()));

    db.execute_raw(TOUCH_CACHED_PATH_SIGNATURE_FETCH.bind([now_val, cache_val, hash_val]))
        .await?;
    Ok(())
}

#[derive(Debug, FromQueryResult)]
pub struct CacheTotals {
    pub total_nars: i64,
    pub total_nar_size: Option<i64>,
    pub total_file_size: Option<i64>,
    pub last_uploaded_at: Option<NaiveDateTime>,
    pub oldest_fetched_at: Option<NaiveDateTime>,
}

pub async fn totals_of_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<Option<CacheTotals>, DbErr> {
    CacheTotals::find_by_statement(NAR_STATS.bind([Value::Uuid(Some(cache.into_inner()))]))
        .one(db)
        .await
}

fn nars_count_sql(where_sql: &str) -> String {
    format!(
        "SELECT COUNT(*) AS total \
         FROM cached_path_signature cps \
         JOIN cached_path cp ON cp.id = cps.cached_path \
         WHERE {where_sql}"
    )
}

fn nars_select_sql(
    where_sql: &str,
    sort_col: &str,
    order_dir: &str,
    limit_idx: usize,
    offset_idx: usize,
) -> String {
    format!(
        "SELECT cp.hash, cp.hash || '-' || cp.package AS store_path, cp.package, \
                cp.nar_size, cp.file_size, cp.created_at, cps.last_fetched_at, \
                cps.pinned_at, u.username AS pinned_by_username \
         FROM cached_path_signature cps \
         JOIN cached_path cp ON cp.id = cps.cached_path \
         LEFT JOIN \"user\" u ON u.id = cps.pinned_by_user_id \
         WHERE {where_sql} \
         ORDER BY {sort_col} {order_dir} NULLS LAST, cp.id ASC \
         LIMIT ${limit_idx} OFFSET ${offset_idx}"
    )
}

// The package filter is a substring match that no index can answer. The cache scan is the plan, not
// a missing index.
crate::sql_fn! {
    NARS_LIST_COUNT = || nars_count_sql("cps.cache = $1 AND cp.hash LIKE $2 AND cp.package LIKE $3"),
        params = [CacheId, Text("abc%"), Text("%hello%")],
        tier = Bulk,
        budget = crate::sql::Budget::bulk().seq_scan_allowed();

    NARS_LIST_SELECT = || nars_select_sql(
        "cps.cache = $1 AND cp.hash LIKE $2 AND cp.package LIKE $3",
        "cp.created_at",
        "DESC",
        4,
        5,
    ),
        params = [CacheId, Text("abc%"), Text("%hello%"), Int(50), Int(0)],
        tier = Bulk,
        budget = crate::sql::Budget::bulk().seq_scan_allowed();
}

#[derive(Debug, Clone, Copy)]
pub struct NarFilter<'a> {
    pub cache: CacheId,
    pub hash_prefix: Option<&'a str>,
    pub package: Option<&'a str>,
    pub pinned: Option<bool>,
}

impl NarFilter<'_> {
    fn conditions(&self) -> (String, Vec<Value>) {
        let mut where_clauses = vec!["cps.cache = $1".to_string()];
        let mut values: Vec<Value> = vec![Value::Uuid(Some(self.cache.into_inner()))];

        if let Some(prefix) = self.hash_prefix {
            let n = values.len() + 1;
            where_clauses.push(format!("cp.hash LIKE ${n}"));
            values.push(Value::String(Some(format!("{prefix}%"))));
        }
        if let Some(needle) = self.package {
            let n = values.len() + 1;
            where_clauses.push(format!("cp.package LIKE ${n}"));
            values.push(Value::String(Some(format!("%{needle}%"))));
        }
        match self.pinned {
            Some(true) => where_clauses.push("cps.pinned_at IS NOT NULL".to_string()),
            Some(false) => where_clauses.push("cps.pinned_at IS NULL".to_string()),
            None => {}
        }

        (where_clauses.join(" AND "), values)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum NarOrder {
    CreatedAt,
    PackageName,
    NarSize,
    FileSize,
    LastFetchedAt,
    PinnedAt,
}

impl NarOrder {
    fn column(self) -> &'static str {
        match self {
            Self::CreatedAt => "cp.created_at",
            Self::PackageName => "cp.package",
            Self::NarSize => "cp.nar_size",
            Self::FileSize => "cp.file_size",
            Self::LastFetchedAt => "cps.last_fetched_at",
            Self::PinnedAt => "cps.pinned_at",
        }
    }
}

crate::sql! {
    PIN_CACHE_CLAIMS = "UPDATE cached_path_signature s \
         SET pinned_at = $1, pinned_by_user_id = $2 \
         FROM cached_path cp \
         WHERE cp.id = s.cached_path AND s.cache = $3 \
           AND cp.hash = ANY($4::text[]) AND s.pinned_at IS NULL",
        params = [Now, UserId, CacheId, CachedPathHashes(64)],
        budget = crate::sql::Budget::hot().buffers(3_000)
            .because("pins update ≤64 signature rows, set pinned_by_user_id, so not heap-only, \
                      seven indexes take writes per row and unique \
                      idx-cached_path_signature-cache-store_path_hash writes land past Hot's \
                      2000-buffer default");

    UNPIN_CACHE_CLAIMS = "UPDATE cached_path_signature s \
         SET pinned_at = NULL, pinned_by_user_id = NULL \
         FROM cached_path cp \
         WHERE cp.id = s.cached_path AND s.cache = $1 \
           AND cp.hash = ANY($2::text[]) AND s.pinned_at IS NOT NULL",
        params = [CacheId, CachedPathHashes(64)];

    SELECT_CLAIMED_CACHED_PATHS_FOR_CACHE_AND_STORE_PATH_HASHES_ANY_SIGNATURE = "SELECT cp.hash, cp.\"references\" \
         FROM cached_path cp JOIN cached_path_signature s ON s.cached_path = cp.id \
         WHERE s.cache = $1 AND cp.hash = ANY($2::text[])",
        params = [CacheId, CachedPathHashes(64)];

    SELECT_CLAIMED_CACHED_PATH_ROW_FOR_CACHE_AND_STORE_PATH_HASH_ANY_SIGNATURE = "SELECT cp.* \
         FROM cached_path cp JOIN cached_path_signature s ON s.cached_path = cp.id \
         WHERE s.cache = $1 AND cp.hash = $2 LIMIT 1",
        params = [CacheId, CachedPathHash];
}

pub async fn find_cached_path_for_cache_and_store_path_hash_ignoring_signature_and_active<
    C: ConnectionTrait,
>(
    db: &C,
    cache: CacheId,
    hash: &str,
) -> Result<Option<MCachedPath>, DbErr> {
    ECachedPath::find()
        .from_raw_sql(
            SELECT_CLAIMED_CACHED_PATH_ROW_FOR_CACHE_AND_STORE_PATH_HASH_ANY_SIGNATURE
                .bind([Value::Uuid(Some(cache.into_inner())), hash.into()]),
        )
        .one(db)
        .await
}

pub async fn collect_store_path_hashes_held_by_cache_for_closure<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    hash: &str,
) -> Result<Vec<String>, DbErr> {
    let mut reached = std::collections::HashSet::from([hash.to_owned()]);
    let mut held = std::collections::BTreeSet::new();
    let mut frontier = vec![hash.to_owned()];
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for chunk in frontier.chunks(crate::IN_CHUNK_SIZE) {
            let rows = db
                .query_all_raw(
                    SELECT_CLAIMED_CACHED_PATHS_FOR_CACHE_AND_STORE_PATH_HASHES_ANY_SIGNATURE
                        .bind([Value::Uuid(Some(cache.into_inner())), chunk.to_vec().into()]),
                )
                .await?;
            for row in rows {
                let references: Option<String> = row.try_get("", "references")?;
                next.extend(
                    references
                        .as_deref()
                        .unwrap_or_default()
                        .split_whitespace()
                        .filter_map(crate::graph::runtime_closure::parse_reference_hash)
                        .filter(|reference| reached.insert(reference.clone())),
                );
                held.insert(row.try_get::<String>("", "hash")?);
            }
        }
        frontier = next;
    }
    Ok(held.into_iter().collect())
}

pub async fn set_claim_pin_timestamp_and_user_for_cached_path_hashes<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    user: UserId,
    hashes: &[String],
) -> Result<u64, DbErr> {
    let pinned_at = gradient_types::now();
    let mut changed = 0;
    for chunk in hashes.chunks(crate::IN_CHUNK_SIZE) {
        changed += db
            .execute_raw(PIN_CACHE_CLAIMS.bind([
                Value::ChronoDateTime(Some(pinned_at)),
                Value::Uuid(Some(user.into_inner())),
                Value::Uuid(Some(cache.into_inner())),
                chunk.to_vec().into(),
            ]))
            .await?
            .rows_affected();
    }
    Ok(changed)
}

pub async fn clear_cache_claim_pin_for_cached_path_hashes<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    hashes: &[String],
) -> Result<u64, DbErr> {
    let mut changed = 0;
    for chunk in hashes.chunks(crate::IN_CHUNK_SIZE) {
        changed += db
            .execute_raw(
                UNPIN_CACHE_CLAIMS
                    .bind([Value::Uuid(Some(cache.into_inner())), chunk.to_vec().into()]),
            )
            .await?
            .rows_affected();
    }
    Ok(changed)
}

pub async fn count_in_cache<C: ConnectionTrait>(
    db: &C,
    filter: NarFilter<'_>,
) -> Result<u64, DbErr> {
    #[derive(FromQueryResult)]
    struct CountRow {
        total: i64,
    }

    let (where_sql, values) = filter.conditions();
    Ok(
        CountRow::find_by_statement(NARS_LIST_COUNT.bind_built(nars_count_sql(&where_sql), values))
            .one(db)
            .await?
            .map(|r| r.total.max(0) as u64)
            .unwrap_or(0),
    )
}

pub async fn page_in_cache<C: ConnectionTrait>(
    db: &C,
    filter: NarFilter<'_>,
    order: NarOrder,
    ascending: bool,
    limit: u64,
    offset: u64,
) -> Result<Vec<NarSummary>, DbErr> {
    let (where_sql, mut values) = filter.conditions();
    let order_dir = if ascending { "ASC" } else { "DESC" };
    let limit_idx = values.len() + 1;
    let offset_idx = values.len() + 2;
    let select_sql = nars_select_sql(&where_sql, order.column(), order_dir, limit_idx, offset_idx);
    values.push(Value::BigInt(Some(limit as i64)));
    values.push(Value::BigInt(Some(offset as i64)));

    NarSummary::find_by_statement(NARS_LIST_SELECT.bind_built(select_sql, values))
        .all(db)
        .await
}

impl From<CacheTotals> for gradient_api_types::NarStats {
    fn from(totals: CacheTotals) -> Self {
        Self {
            total_nars: totals.total_nars,
            total_nar_size: totals.total_nar_size.unwrap_or(0),
            total_file_size: totals.total_file_size.unwrap_or(0),
            last_uploaded_at: totals.last_uploaded_at,
            oldest_fetched_at: totals.oldest_fetched_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;

    #[tokio::test]
    async fn migration_up_removes_cached_path_without_file_hash_and_its_signature_and_producer_rows_and_rejects_null_file_hash_insert_and_cascades_foreign_key_deletes()
     {
        use gradient_migration::{Migrator, MigratorTrait};
        use sea_orm::ConnectionTrait;
        use sea_orm_migration::SchemaManager;

        let path_hash_of_seeded_copy_row_with_null_file_hash_before_migration = "4".repeat(32);
        let seeded_copy_row_store_path_hash = "5".repeat(32);
        let cache = gradient_entity::ids::CacheId::now_v7();
        let db = gradient_fixtures::shape::shape()
            .evaluation(gradient_entity::ids::EvaluationId::now_v7())
            .derivation(&format!("/nix/store/{}-migration.drv", "6".repeat(32)))
            .job()
            .cached_path(
                &path_hash_of_seeded_copy_row_with_null_file_hash_before_migration,
                "without-bytes",
            )
            .into_cache(cache)
            .cached_path(&seeded_copy_row_store_path_hash, "with-bytes")
            .into_database()
            .await;
        let connection = db.connection();
        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name().starts_with("m20270102_000000_"))
            .expect("Register a migration to delete copies that have no stored bytes.");
        let manager = SchemaManager::new(&connection);
        migration
            .down(&manager)
            .await
            .expect("The migration rollback ran.");
        db.execute(&format!(
            "UPDATE cached_path SET file_hash = NULL \
             WHERE hash = '{path_hash_of_seeded_copy_row_with_null_file_hash_before_migration}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             SELECT uuidv7(), id, '{cache}', convert_to('signature', 'UTF8'), 0, now() FROM cached_path;"
        ))
        .await;

        migration.up(&manager).await.expect("The migration ran.");
        let cached_path_copy_row_hashes_after_migration =
            db.texts("SELECT hash FROM cached_path ORDER BY hash").await;
        let remaining_signature_and_producer_row_counts_text = db
            .texts(
                "SELECT (SELECT count(*) FROM cached_path_signature)::text || ' ' || \
                 (SELECT count(*) FROM store_path_content_producer)::text",
            )
            .await;
        let insert_cached_path_without_file_hash_result = connection
            .execute_unprepared(&format!(
                "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, nar_hash, nar_size, created_at) \
                 VALUES (uuidv7(), '{}', 'refused', 'refused', 'sha256:refused', 1, now())",
                "7".repeat(32)
            ))
            .await;
        let foreign_keys_referencing_cached_path_with_delete_rules = db
            .texts(
                "SELECT conrelid::regclass::text || ' ' || confdeltype::text FROM pg_constraint \
                 WHERE contype = 'f' AND confrelid = 'cached_path'::regclass ORDER BY 1",
            )
            .await;

        assert_eq!(
            cached_path_copy_row_hashes_after_migration,
            vec![seeded_copy_row_store_path_hash],
            "Remove copies without bytes. Copies with bytes remain."
        );
        assert_eq!(
            remaining_signature_and_producer_row_counts_text,
            vec!["1 1".to_string()],
            "The signature row and producer row of a deleted copy go away with that copy."
        );
        assert!(
            insert_cached_path_without_file_hash_result.is_err(),
            "An insert without `file_hash` fails."
        );
        assert!(
            !foreign_keys_referencing_cached_path_with_delete_rules.is_empty()
                && foreign_keys_referencing_cached_path_with_delete_rules
                    .iter()
                    .all(|rule| rule.ends_with(" c")),
            "All foreign keys that reference `cached_path` specify cascade delete. \
             {foreign_keys_referencing_cached_path_with_delete_rules:?}"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn migration_up_deletes_unconfirmed_cached_path_with_signature_producer_and_debug_info_rows_keeps_confirmed_cached_path_confirmed_column_and_partial_index_rows_without_confirmed_read_true_down_changes_nothing()
     {
        use gradient_migration::{Migrator, MigratorTrait};
        use sea_orm_migration::SchemaManager;

        let unconfirmed_store_path_hash = "8".repeat(32);
        let confirmed_store_path_hash_that_must_survive = "9".repeat(32);
        let cache = gradient_entity::ids::CacheId::now_v7();
        let db = gradient_fixtures::shape::shape()
            .evaluation(gradient_entity::ids::EvaluationId::now_v7())
            .derivation(&format!("/nix/store/{}-migration.drv", "a".repeat(32)))
            .walked()
            .input_source(&format!(
                "/nix/store/{unconfirmed_store_path_hash}-unconfirmed"
            ))
            .job()
            .cached_path(&unconfirmed_store_path_hash, "unconfirmed")
            .into_cache(cache)
            .cached_path(&confirmed_store_path_hash_that_must_survive, "confirmed")
            .into_database()
            .await;
        let connection = db.connection();
        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name().starts_with("m20270103_000000_"))
            .expect("Register a migration to delete unconfirmed copies.");
        let manager = SchemaManager::new(&connection);
        migration
            .down(&manager)
            .await
            .expect("The migration rollback ran.");
        db.execute(&format!(
            "UPDATE cached_path SET confirmed = false WHERE hash = '{unconfirmed_store_path_hash}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             SELECT uuidv7(), id, '{cache}', convert_to('signature', 'UTF8'), 0, now() FROM cached_path; \
             INSERT INTO debug_info (id, build_id, cached_path, member, created_at) \
             SELECT uuidv7(), hash, id, 'lib/debug/' || hash, now() FROM cached_path;"
        ))
        .await;

        migration.up(&manager).await.expect("The migration ran.");
        let remaining_cached_path_hashes_with_dependent_row_counts = db
            .texts(
                "SELECT cp.hash || ' ' || \
                 (SELECT count(*) FROM cached_path_signature s WHERE s.cached_path = cp.id)::text || ' ' || \
                 (SELECT count(*) FROM store_path_content_producer p WHERE p.cached_path = cp.id)::text || ' ' || \
                 (SELECT count(*) FROM debug_info d WHERE d.cached_path = cp.id)::text \
                 FROM cached_path cp ORDER BY cp.hash",
            )
            .await;
        let confirmed_column_and_unconfirmed_index_count_text_after_migration_up = db
            .texts(
                "SELECT (SELECT count(*) FROM information_schema.columns \
                 WHERE table_name = 'cached_path' AND column_name = 'confirmed')::text || ' ' || \
                 (SELECT count(*) FROM pg_indexes WHERE indexname = 'idx-cached_path-unconfirmed')::text",
            )
            .await;
        let confirmed_values_of_remaining_cached_paths =
            db.texts("SELECT confirmed::text FROM cached_path").await;
        let derivation_walked_flag_after_unconfirmed_cached_path_deletion =
            db.texts("SELECT walked::text FROM derivation").await;
        let derivation_row_count_with_changed_walk_counters_after_cached_path_migration =
            crate::graph::walk_completeness::recount_walk_completeness(&connection)
                .await
                .unwrap();
        migration
            .down(&manager)
            .await
            .expect("The migration rollback ran.");
        let confirmed_column_and_unconfirmed_index_count_text_after_migration_down = db
            .texts(
                "SELECT (SELECT count(*) FROM information_schema.columns \
                 WHERE table_name = 'cached_path' AND column_name = 'confirmed')::text || ' ' || \
                 (SELECT count(*) FROM pg_indexes WHERE indexname = 'idx-cached_path-unconfirmed')::text",
            )
            .await;

        assert_eq!(
            remaining_cached_path_hashes_with_dependent_row_counts,
            vec![format!(
                "{confirmed_store_path_hash_that_must_survive} 1 1 1"
            )],
            "Removals delete unverified copies along with their signature row, producer row and debug info row. Verified copies keep one row each."
        );
        assert_eq!(
            confirmed_column_and_unconfirmed_index_count_text_after_migration_up,
            vec!["1 1".to_string()],
            "This migration does not remove the confirmed column or its partial index."
        );
        assert_eq!(
            confirmed_values_of_remaining_cached_paths,
            vec!["true".to_string()],
            "Copies that have no `confirmed` value on insert read back as true."
        );
        assert_eq!(
            (
                derivation_walked_flag_after_unconfirmed_cached_path_deletion,
                derivation_row_count_with_changed_walk_counters_after_cached_path_migration
            ),
            (vec!["false".to_string()], 0),
            "The migration marked as unwalked the derivations that list unconfirmed uploads as input sources. Walk counters stayed exact."
        );
        assert_eq!(
            confirmed_column_and_unconfirmed_index_count_text_after_migration_down,
            vec!["1 1".to_string()],
            "This migration does not remove the confirmed column or its partial index."
        );
        migration.up(&manager).await.expect("The migration ran.");
        db.finish().await;
    }

    #[tokio::test]
    async fn count_fetch_for_hash_with_two_stored_contents_bumps_signature_row_counter() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        let cache = crate::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("the cache is written");
        let signed = CachedPathId::now_v7();
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES ('{signed}', 'twicehash', 'twicehash', 'twice', 'sha256:file-first', 'sha256:first', 1, now()), \
                    (uuidv7(), 'twicehash', 'twicehash-other', 'twice', 'sha256:file-second', 'sha256:second', 1, now()); \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             VALUES (uuidv7(), '{signed}', '{cache}', 0, now());",
            cache = cache.id,
        ))
        .await;

        count_fetch(&conn, cache.id, "twicehash")
            .await
            .expect("the fetch is counted");

        assert_eq!(
            db.texts("SELECT fetch_count::text FROM cached_path_signature")
                .await,
            vec!["1".to_owned()],
            "A signature row in the cache counts as one fetch."
        );
        db.finish().await;
    }

    struct TwoAttemptsOfOneBuild {
        readers: gradient_fixtures::store_path_content_variants::Readers,
        drv_hash: String,
        drv_name: String,
        older_attempt: BuildAttemptId,
    }

    async fn build_a_with_older_attempt_producing_the_cache_one_variant_and_newer_unsigned_rebuild()
    -> TwoAttemptsOfOneBuild {
        use gradient_fixtures::store_path_content_variants::{
            BUILD_A, FIRST_KEY, SHARED, two_variants_of_one_hash_and_one_untrusted_hash,
        };

        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let build = readers.db.shared_build(BUILD_A).await;
        let (older_job, newer_job) = (DispatchedJobId::now_v7(), DispatchedJobId::now_v7());
        let (older_attempt, newer_attempt) = (BuildAttemptId::now_v7(), BuildAttemptId::now_v7());
        readers
            .db
            .execute(&format!(
                "INSERT INTO derivation_output (id, derivation, name, hash, package, path_known_after_build, created_at) \
                 VALUES (uuidv7(), '{derivation}', 'out', '{SHARED}', 'shared', false, now()); \
                 INSERT INTO dispatched_job (id, kind, evaluation_id, project, worker_id, score, queued_at, \
                                             dispatched_at, score_breakdown, worker_context, job_context, created_at) \
                 SELECT ids.id, 1, job.evaluation, '{project}', 'worker', 0, ids.at, ids.at, '{{}}', '{{}}', '{{}}', ids.at \
                 FROM (VALUES ('{older_job}'::uuid, now() - interval '1 hour'), ('{newer_job}'::uuid, now())) ids(id, at) \
                 CROSS JOIN (SELECT evaluation FROM build_job WHERE derivation_build = '{build}' LIMIT 1) job; \
                 INSERT INTO build_attempt (id, derivation_build, dispatched_job, substitute, outcome, build_context, created_at) \
                 VALUES ('{older_attempt}', '{build}', '{older_job}', false, 1, '{{}}', now() - interval '1 hour'), \
                        ('{newer_attempt}', '{build}', '{newer_job}', false, 1, '{{}}', now()); \
                 UPDATE store_path_content_producer SET dispatched_job = '{older_job}' \
                 WHERE cached_path = (SELECT id FROM cached_path WHERE nar_object_storage_key = '{FIRST_KEY}'); \
                 INSERT INTO cached_path (id, hash, nar_object_storage_key, package, nar_hash, nar_size, file_hash, created_at) \
                 VALUES (uuidv7(), '{SHARED}', 'rebuilt-by-newer-attempt', 'shared', 'sha256:rebuilt', 1, 'sha256:file', now()); \
                 INSERT INTO store_path_content_producer (id, cached_path, kind, team_worker, dispatched_job, created_at) \
                 SELECT uuidv7(), rebuilt.id, built.kind, built.team_worker, '{newer_job}', now() \
                 FROM cached_path rebuilt, store_path_content_producer built \
                 WHERE rebuilt.nar_object_storage_key = 'rebuilt-by-newer-attempt' \
                   AND built.dispatched_job = '{older_job}';",
                derivation = build.derivation,
                project = readers.project_a,
                build = build.id,
            ))
            .await;
        let named = readers
            .db
            .texts(&format!(
                "SELECT hash || ' ' || name FROM derivation WHERE id = '{}'",
                build.derivation
            ))
            .await;
        let (drv_hash, drv_name) = named[0].split_once(' ').expect("Hash and Name");
        TwoAttemptsOfOneBuild {
            drv_hash: drv_hash.to_owned(),
            drv_name: drv_name.to_owned(),
            readers,
            older_attempt,
        }
    }

    #[tokio::test]
    async fn build_log_attempt_of_a_cache_is_the_attempt_that_produced_the_variant_it_serves_not_the_latest()
     {
        let build =
            build_a_with_older_attempt_producing_the_cache_one_variant_and_newer_unsigned_rebuild()
                .await;

        let found = build_attempt_that_produced_derivation_output_served_by_cache(
            &build.readers.db.connection(),
            build.readers.cache_one,
            &build.drv_hash,
            &build.drv_name,
        )
        .await
        .expect("The lookup ran");

        assert_eq!(
            found,
            Some(build.older_attempt),
            "Cache C1 serves the variant of the older attempt, so its log is the log of that attempt."
        );
        build.readers.db.finish().await;
    }

    #[tokio::test]
    async fn build_log_attempt_of_a_cache_serving_an_uploaded_variant_is_none() {
        let build =
            build_a_with_older_attempt_producing_the_cache_one_variant_and_newer_unsigned_rebuild()
                .await;

        let found = build_attempt_that_produced_derivation_output_served_by_cache(
            &build.readers.db.connection(),
            build.readers.cache_two,
            &build.drv_hash,
            &build.drv_name,
        )
        .await
        .expect("The lookup ran");

        assert_eq!(
            found, None,
            "Cache C2 serves a variant a user uploaded, which no build attempt produced."
        );
        build.readers.db.finish().await;
    }

    #[tokio::test]
    async fn cache_build_log_query_returns_nothing_for_attempt_of_project_that_does_not_read_cache_then_returns_attempt_of_project_that_reads_cache()
     {
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use gradient_fixtures::store_path_content_variants::BUILD_A;

        let build =
            build_a_with_older_attempt_producing_the_cache_one_variant_and_newer_unsigned_rebuild()
                .await;
        let readers = &build.readers;
        let conn = readers.db.connection();
        EProjectCache::update_many()
            .col_expr(
                CProjectCache::Mode,
                sea_orm::sea_query::Expr::value(CacheSubscriptionMode::WriteOnly),
            )
            .filter(CProjectCache::Project.eq(readers.project_a))
            .filter(CProjectCache::Cache.eq(readers.cache_one))
            .exec(&conn)
            .await
            .expect("project A only writes into cache C1");
        let lookup = || {
            build_attempt_that_produced_derivation_output_served_by_cache(
                &conn,
                readers.cache_one,
                &build.drv_hash,
                &build.drv_name,
            )
        };

        assert_eq!(
            lookup().await.expect("The lookup ran"),
            None,
            "Project A can write to cache C1 but can't read from it. A is the only project with a build job for that derivation, so no build logs come from C1."
        );

        let evaluation_b = readers
            .db
            .texts(&format!(
                "SELECT evaluation::text FROM build_job WHERE derivation_build = '{}'",
                readers.build_b
            ))
            .await;
        crate::rows::insert::<EProjectCache, _>(
            &conn,
            gradient_entity::project_cache::Model {
                id: ProjectCacheId::now_v7(),
                project: readers.project_b,
                cache: readers.cache_one,
                mode: CacheSubscriptionMode::ReadOnly,
                sign_outputs: false,
                ..Default::default()
            },
        )
        .await
        .expect("project B reads cache C1");
        crate::rows::insert::<EBuildJob, _>(
            &conn,
            gradient_entity::build_job::Model {
                id: BuildJobId::now_v7(),
                evaluation: EvaluationId::new(evaluation_b[0].parse().expect("an evaluation id")),
                derivation: readers.db.shared_build(BUILD_A).await.derivation,
                derivation_build: readers.build_a,
                score_breakdown: serde_json::json!({}),
                created_at: gradient_fixtures::test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("project B has a build job of the derivation");

        assert_eq!(
            lookup().await.expect("The lookup ran"),
            Some(build.older_attempt),
            "Project B can read cache C1. Project B can have a build job for the same derivation, so C1 can provide the log of the attempt that produced that variant."
        );
        build.readers.db.finish().await;
    }

    async fn set_up_project_a_as_cache_writer_and_project_b_as_reader_with_build_job_for_project_a_creation_time_and_project_b_evaluation_finish_time(
        build: &TwoAttemptsOfOneBuild,
        build_job_created_at: &str,
        evaluation_finished_at: &str,
    ) {
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use gradient_fixtures::store_path_content_variants::BUILD_A;

        let readers = &build.readers;
        let conn = readers.db.connection();
        EProjectCache::update_many()
            .col_expr(
                CProjectCache::Mode,
                sea_orm::sea_query::Expr::value(CacheSubscriptionMode::WriteOnly),
            )
            .filter(CProjectCache::Project.eq(readers.project_a))
            .filter(CProjectCache::Cache.eq(readers.cache_one))
            .exec(&conn)
            .await
            .expect("project A only writes into cache C1");
        let evaluation_b = readers
            .db
            .texts(&format!(
                "SELECT evaluation::text FROM build_job WHERE derivation_build = '{}'",
                readers.build_b
            ))
            .await;
        crate::rows::insert::<EProjectCache, _>(
            &conn,
            gradient_entity::project_cache::Model {
                id: ProjectCacheId::now_v7(),
                project: readers.project_b,
                cache: readers.cache_one,
                mode: CacheSubscriptionMode::ReadOnly,
                sign_outputs: false,
                ..Default::default()
            },
        )
        .await
        .expect("project B reads cache C1");
        let job = crate::rows::insert::<EBuildJob, _>(
            &conn,
            gradient_entity::build_job::Model {
                id: BuildJobId::now_v7(),
                evaluation: EvaluationId::new(evaluation_b[0].parse().expect("an evaluation id")),
                derivation: readers.db.shared_build(BUILD_A).await.derivation,
                derivation_build: readers.build_a,
                score_breakdown: serde_json::json!({}),
                created_at: gradient_fixtures::test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("project B has a build job of the derivation");
        readers
            .db
            .execute(&format!(
                "UPDATE build_job SET created_at = {build_job_created_at} WHERE id = '{job}'; \
                 UPDATE evaluation SET finished_at = {evaluation_finished_at} WHERE id = '{evaluation}'",
                job = job.id,
                evaluation = evaluation_b[0],
            ))
            .await;
    }

    async fn build_attempt_id_served_by_cache_c1(
        build: &TwoAttemptsOfOneBuild,
    ) -> Option<BuildAttemptId> {
        build_attempt_that_produced_derivation_output_served_by_cache(
            &build.readers.db.connection(),
            build.readers.cache_one,
            &build.drv_hash,
            &build.drv_name,
        )
        .await
        .expect("The lookup ran")
    }

    #[tokio::test]
    async fn cache_build_log_query_returns_nothing_when_project_build_job_was_in_evaluation_finished_before_build_attempt_created_and_attempt_was_dispatched_only_for_project_that_does_not_read_cache()
     {
        let build =
            build_a_with_older_attempt_producing_the_cache_one_variant_and_newer_unsigned_rebuild()
                .await;
        set_up_project_a_as_cache_writer_and_project_b_as_reader_with_build_job_for_project_a_creation_time_and_project_b_evaluation_finish_time(
            &build,
            "now() - interval '3 hours'",
            "now() - interval '2 hours'",
        )
        .await;

        assert_eq!(
            build_attempt_id_served_by_cache_c1(&build).await,
            None,
            "Project B can read cache C1, but B's evaluation finished before the attempt that produced the served variant started. That attempt ran only for Project A, which cannot read C1. C1 cannot display a log."
        );
        build.readers.db.finish().await;
    }

    #[tokio::test]
    async fn cache_build_log_query_returns_log_from_cache_when_reading_project_build_job_is_created_after_build_attempt_finished()
     {
        let build =
            build_a_with_older_attempt_producing_the_cache_one_variant_and_newer_unsigned_rebuild()
                .await;
        build
            .readers
            .db
            .execute(&format!(
                "UPDATE build_attempt SET build_finished_at = now() - interval '50 minutes' WHERE id = '{}'",
                build.older_attempt
            ))
            .await;
        set_up_project_a_as_cache_writer_and_project_b_as_reader_with_build_job_for_project_a_creation_time_and_project_b_evaluation_finish_time(
            &build,
            "now() - interval '10 minutes'",
            "now()",
        )
        .await;

        assert_eq!(
            build_attempt_id_served_by_cache_c1(&build).await,
            Some(build.older_attempt),
        );
        build.readers.db.finish().await;
    }

    #[tokio::test]
    async fn cache_build_log_query_returns_log_when_build_job_was_created_during_build_attempt() {
        let build =
            build_a_with_older_attempt_producing_the_cache_one_variant_and_newer_unsigned_rebuild()
                .await;
        build
            .readers
            .db
            .execute(&format!(
                "UPDATE build_attempt SET build_finished_at = now() - interval '30 minutes' WHERE id = '{}'",
                build.older_attempt
            ))
            .await;
        set_up_project_a_as_cache_writer_and_project_b_as_reader_with_build_job_for_project_a_creation_time_and_project_b_evaluation_finish_time(
            &build,
            "now() - interval '45 minutes'",
            "now() - interval '20 minutes'",
        )
        .await;

        assert_eq!(
            build_attempt_id_served_by_cache_c1(&build).await,
            Some(build.older_attempt),
        );
        build.readers.db.finish().await;
    }

    #[tokio::test]
    async fn cache_build_log_query_returns_nothing_when_cache_is_inactive() {
        let build =
            build_a_with_older_attempt_producing_the_cache_one_variant_and_newer_unsigned_rebuild()
                .await;
        build
            .readers
            .db
            .execute(&format!(
                "UPDATE cache SET active = false WHERE id = '{}'",
                build.readers.cache_one
            ))
            .await;

        let found = build_attempt_that_produced_derivation_output_served_by_cache(
            &build.readers.db.connection(),
            build.readers.cache_one,
            &build.drv_hash,
            &build.drv_name,
        )
        .await
        .expect("The lookup ran");

        assert_eq!(
            found, None,
            "No build logs appear because cache C1 is inactive."
        );
        build.readers.db.finish().await;
    }

    fn registration_of_the_worker_cache_one_does_not_trust() -> String {
        format!(
            "(SELECT worker_registration FROM store_path_content_producer \
              WHERE cached_path = (SELECT id FROM cached_path WHERE hash = '{}'))",
            gradient_fixtures::store_path_content_variants::UNTRUSTED,
        )
    }

    #[tokio::test]
    async fn build_log_attempt_of_a_cache_skips_a_newer_attempt_of_a_producer_the_cache_does_not_trust()
     {
        use gradient_fixtures::store_path_content_variants::FIRST_KEY;

        let build =
            build_a_with_older_attempt_producing_the_cache_one_variant_and_newer_unsigned_rebuild()
                .await;
        build
            .readers
            .db
            .execute(&format!(
                "INSERT INTO store_path_content_producer (id, cached_path, kind, worker_registration, dispatched_job, created_at) \
                 SELECT uuidv7(), served.id, 0, {untrusted}, newer.dispatched_job, now() \
                 FROM cached_path served, build_attempt newer \
                 WHERE served.nar_object_storage_key = '{FIRST_KEY}' AND newer.id <> '{older}'",
                untrusted = registration_of_the_worker_cache_one_does_not_trust(),
                older = build.older_attempt,
            ))
            .await;

        let found = build_attempt_that_produced_derivation_output_served_by_cache(
            &build.readers.db.connection(),
            build.readers.cache_one,
            &build.drv_hash,
            &build.drv_name,
        )
        .await
        .expect("The lookup ran");

        assert_eq!(
            found,
            Some(build.older_attempt),
            "A worker that cache C1 does not trust rebuilt the served variant bit for bit, so its newer attempt is not the log of C1."
        );
        build.readers.db.finish().await;
    }

    #[tokio::test]
    async fn build_log_attempt_of_a_cache_is_none_when_the_variant_it_serves_through_an_upload_was_built_only_by_an_untrusted_worker()
     {
        use gradient_fixtures::store_path_content_variants::FIRST_KEY;

        let build =
            build_a_with_older_attempt_producing_the_cache_one_variant_and_newer_unsigned_rebuild()
                .await;
        build
            .readers
            .db
            .execute(&format!(
                "UPDATE store_path_content_producer SET team_worker = NULL, worker_registration = {untrusted} \
                 WHERE cached_path = (SELECT id FROM cached_path WHERE nar_object_storage_key = '{FIRST_KEY}'); \
                 INSERT INTO store_path_content_producer (id, cached_path, kind, cache, upload_channel, created_at) \
                 SELECT uuidv7(), id, 1, '{cache}', 0, now() FROM cached_path WHERE nar_object_storage_key = '{FIRST_KEY}';",
                untrusted = registration_of_the_worker_cache_one_does_not_trust(),
                cache = build.readers.cache_one,
            ))
            .await;

        let found = build_attempt_that_produced_derivation_output_served_by_cache(
            &build.readers.db.connection(),
            build.readers.cache_one,
            &build.drv_hash,
            &build.drv_name,
        )
        .await
        .expect("The lookup ran");

        assert_eq!(
            found, None,
            "Cache C1 serves the variant because it was uploaded into C1; the only attempt that built it ran on a worker C1 does not trust."
        );
        build.readers.db.finish().await;
    }

    #[tokio::test]
    async fn binary_cache_not_trusting_only_output_variant_makes_build_attempt_lookup_return_none()
    {
        use gradient_fixtures::store_path_content_variants::{
            BUILD_A, UNTRUSTED, two_variants_of_one_hash_and_one_untrusted_hash,
        };

        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let derivation = readers.db.shared_build(BUILD_A).await.derivation;
        readers
            .db
            .execute(&format!(
                "INSERT INTO derivation_output (id, derivation, name, hash, package, path_known_after_build, created_at) \
                 VALUES (uuidv7(), '{derivation}', 'untrusted', '{UNTRUSTED}', 'untrusted', false, now())"
            ))
            .await;
        let derivation_hash_and_name_rows = readers
            .db
            .texts(&format!(
                "SELECT hash || ' ' || name FROM derivation WHERE id = '{derivation}'"
            ))
            .await;
        let (drv_hash, drv_name) = derivation_hash_and_name_rows[0]
            .split_once(' ')
            .expect("Hash and Name");

        let build_attempt_that_produced_cached_derivation_output =
            build_attempt_that_produced_derivation_output_served_by_cache(
                &readers.db.connection(),
                readers.cache_one,
                drv_hash,
                drv_name,
            )
            .await
            .expect("The lookup ran");

        assert_eq!(
            build_attempt_that_produced_cached_derivation_output, None,
            "Cache C1 has the output signature. Cache C1 doesn't trust the only variant, so the cache serves neither that derivation nor its build log."
        );
        readers.db.finish().await;
    }
}
