/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::{CacheId, EvaluationId};
use gradient_fixtures::shape::shape;
use gradient_migration::{Migrator, MigratorTrait};

const PER_CACHE_DELETION_COUNT_MIGRATION_NAME: &str = "m20270109_000000_create_per_cache_cached_path_signature_deletion_count_table_with_delete_trigger_and_add_claimed_deletion_count_drop_references_are_claimed";

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
    let migrations = Migrator::migrations();
    let steps = migrations.len()
        - migrations
            .iter()
            .position(|migration| migration.name() == PER_CACHE_DELETION_COUNT_MIGRATION_NAME)
            .expect("Entries in the migration list include the migration for the deletion count.");
    Migrator::down(&db, Some(u32::try_from(steps).expect("step count")))
        .await
        .expect("Revert the migration for deletion counts.");
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
    let new_column_values = database
        .texts(&format!(
            "SELECT coalesce(reference_closure_established_at_cache_deletion_count::text, 'NULL') \
             FROM cached_path_signature WHERE cache = '{cache}'"
        ))
        .await;
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
