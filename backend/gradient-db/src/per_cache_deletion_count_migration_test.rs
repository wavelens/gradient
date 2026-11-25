/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::{CacheId, EvaluationId};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::user_id;
use gradient_fixtures::rows::cache_with_id;
use gradient_fixtures::shape::shape;
use gradient_migration::{Migrator, MigratorTrait};
use sea_orm::{ActiveModelTrait, DatabaseConnection, IntoActiveModel};

const PER_CACHE_DELETION_COUNT_MIGRATION_NAME: &str = "m20270109_000000_create_per_cache_cached_path_signature_deletion_count_table_with_delete_trigger_and_add_claimed_deletion_count_drop_references_are_claimed";

const PER_CACHE_DELETION_COUNT_GIN_INDEX_BACKFILL_AND_TRIGGER_MIGRATION_NAME: &str = "m20270110_000000_add_gin_index_on_referenced_store_path_hashes_create_per_cache_reference_closure_count_row_with_cache_insert_trigger_set_stale_reference_closure_marks_to_null_and_replace_delete_trigger_to_clear_marks_above_deleted_path";

async fn revert_migrations_from_named_to_newest(db: &DatabaseConnection, migration_name: &str) {
    let migrations = Migrator::migrations();
    let steps = migrations.len()
        - migrations
            .iter()
            .position(|migration| migration.name() == migration_name)
            .expect("Entries in the migration list include the migration with the given name.");
    Migrator::down(db, Some(u32::try_from(steps).expect("step count")))
        .await
        .expect("Revert the migration for deletion counts.");
}

async fn cached_path_signature_marks_for_cache_sorted_by_store_path_hash(
    database: &TestDatabase,
    cache: CacheId,
) -> Vec<String> {
    database
        .texts(&format!(
            "SELECT coalesce(reference_closure_established_at_cache_deletion_count::text, 'NULL') \
             FROM cached_path_signature WHERE cache = '{cache}' ORDER BY store_path_hash"
        ))
        .await
}

#[tokio::test]
async fn migration_up_after_cached_path_signature_references_are_claimed_removes_old_column_sets_new_column_null_and_counts_delete_trigger()
 {
    let hash = "t".repeat(32);
    let cache = CacheId::now_v7();
    let database = shape()
        .evaluation(EvaluationId::now_v7())
        .into_cache(cache)
        .keyed("signing-key")
        .cached_path(&hash, "path")
        .into_database()
        .await;
    let db = database.connection();
    revert_migrations_from_named_to_newest(&db, PER_CACHE_DELETION_COUNT_MIGRATION_NAME).await;
    database
        .execute(&format!(
            "INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at, references_are_claimed) \
             SELECT uuidv7(), cp.id, '{cache}', 0, now(), true FROM cached_path cp WHERE cp.hash = '{hash}'"
        ))
        .await;

    Migrator::up(&db, None)
        .await
        .expect("Apply the migration for deletion counts again.");
    let old_columns = database
        .texts(
            "SELECT column_name::text FROM information_schema.columns \
             WHERE table_name = 'cached_path_signature' AND column_name = 'references_are_claimed'",
        )
        .await;
    let new_column_values =
        cached_path_signature_marks_for_cache_sorted_by_store_path_hash(&database, cache).await;
    database
        .execute(&format!(
            "DELETE FROM cached_path_signature WHERE cache = '{cache}'"
        ))
        .await;
    let deletion_counts = database
        .texts(&format!(
            "SELECT deleted_cached_path_signature_count::text \
             FROM binary_cache_deleted_path_signature_count WHERE cache = '{cache}'"
        ))
        .await;
    database.finish().await;
    assert_eq!(
        (old_columns, new_column_values, deletion_counts),
        (
            Vec::<String>::new(),
            vec!["NULL".to_owned()],
            vec!["1".to_owned()]
        ),
        "The references_are_claimed column is absent from cached_path_signature after migration. Rows with references_are_claimed = true have NULL in reference_closure_established_at_cache_deletion_count. Delete that row -> the deletion count of that cache is 1."
    );
}

#[tokio::test]
async fn test_migration_m20270110_backfills_cache_deletion_counts_and_nulls_mismatched_cached_path_signature_marks()
 {
    let [
        path_hash_where_stored_mark_equals_cache_deletion_count,
        store_path_hash_with_mismatched_deletion_mark,
    ] = ["m", "s"].map(|c| c.repeat(32));
    let (
        cache_id_with_deleted_deletion_count_row,
        cache_with_deletion_count_set_to_two_before_migration,
    ) = (CacheId::now_v7(), CacheId::now_v7());
    let database = shape()
        .evaluation(EvaluationId::now_v7())
        .into_cache(cache_id_with_deleted_deletion_count_row)
        .keyed("signing-key")
        .into_cache(cache_with_deletion_count_set_to_two_before_migration)
        .keyed("signing-key")
        .cached_path(
            &path_hash_where_stored_mark_equals_cache_deletion_count,
            "path",
        )
        .cached_path(&store_path_hash_with_mismatched_deletion_mark, "path")
        .into_database()
        .await;
    let db = database.connection();
    revert_migrations_from_named_to_newest(
        &db,
        PER_CACHE_DELETION_COUNT_GIN_INDEX_BACKFILL_AND_TRIGGER_MIGRATION_NAME,
    )
    .await;
    database
        .execute(&format!(
            "DELETE FROM binary_cache_deleted_path_signature_count WHERE cache = '{cache_id_with_deleted_deletion_count_row}'; \
             INSERT INTO binary_cache_deleted_path_signature_count (cache, deleted_cached_path_signature_count) \
             VALUES ('{cache_with_deletion_count_set_to_two_before_migration}', 2) \
             ON CONFLICT (cache) DO UPDATE SET deleted_cached_path_signature_count = 2; \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at, reference_closure_established_at_cache_deletion_count) \
             SELECT uuidv7(), cp.id, marked.cache, 0, now(), marked.mark \
             FROM cached_path cp JOIN (VALUES \
                 ('{path_hash_where_stored_mark_equals_cache_deletion_count}', '{cache_id_with_deleted_deletion_count_row}'::uuid, 0::bigint), \
                 ('{store_path_hash_with_mismatched_deletion_mark}', '{cache_id_with_deleted_deletion_count_row}'::uuid, 1::bigint), \
                 ('{path_hash_where_stored_mark_equals_cache_deletion_count}', '{cache_with_deletion_count_set_to_two_before_migration}'::uuid, 2::bigint), \
                 ('{store_path_hash_with_mismatched_deletion_mark}', '{cache_with_deletion_count_set_to_two_before_migration}'::uuid, 1::bigint) \
             ) marked(hash, cache, mark) ON marked.hash = cp.hash"
        ))
        .await;

    Migrator::up(&db, None)
        .await
        .expect("Apply the migration again.");
    let cache_id_inserted_after_migration = CacheId::now_v7();
    cache_with_id(
        cache_id_inserted_after_migration,
        "cache-inserted-after-migration",
        user_id(),
    )
    .into_active_model()
    .insert(&db)
    .await
    .expect("Insert the cache after the migration.");
    let cache_deletion_counts_after_migration = database
        .texts(&format!(
            "SELECT coalesce(counted.deleted_cached_path_signature_count::text, 'none') \
             FROM unnest(ARRAY['{cache_id_with_deleted_deletion_count_row}', \
                               '{cache_with_deletion_count_set_to_two_before_migration}', \
                               '{cache_id_inserted_after_migration}']::uuid[]) WITH ORDINALITY listed(cache, position) \
             LEFT JOIN binary_cache_deleted_path_signature_count counted ON counted.cache = listed.cache \
             ORDER BY listed.position"
        ))
        .await;
    let store_marks_of_cache_rows_without_count_row =
        cached_path_signature_marks_for_cache_sorted_by_store_path_hash(
            &database,
            cache_id_with_deleted_deletion_count_row,
        )
        .await;
    let stored_marks_or_null_of_cache_rows_with_count_two_sorted_by_store_path_hash =
        cached_path_signature_marks_for_cache_sorted_by_store_path_hash(
            &database,
            cache_with_deletion_count_set_to_two_before_migration,
        )
        .await;
    database.finish().await;
    assert_eq!(
        (
            cache_deletion_counts_after_migration,
            store_marks_of_cache_rows_without_count_row,
            stored_marks_or_null_of_cache_rows_with_count_two_sorted_by_store_path_hash
        ),
        (
            vec!["0".to_owned(), "2".to_owned(), "0".to_owned()],
            vec!["0".to_owned(), "NULL".to_owned()],
            vec!["2".to_owned(), "NULL".to_owned()]
        ),
        "The up migration made the deletion count 0 for caches without a count and kept count 2. A cache inserted later had deletion count 0. A mark remained when it equaled the current deletion count of that cache. Other marks became NULL."
    );
}
