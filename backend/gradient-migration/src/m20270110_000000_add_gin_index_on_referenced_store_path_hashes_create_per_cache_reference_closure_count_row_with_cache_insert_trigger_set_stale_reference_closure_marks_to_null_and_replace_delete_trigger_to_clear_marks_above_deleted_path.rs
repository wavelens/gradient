/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

use crate::sql_on_one_connection::run_sql_statements_on_one_connection;

const LOCK_TIMEOUT_SQL: &str = "SET lock_timeout = '5s';";

const CREATE_OR_REPLACE_CACHED_PATH_REFERENCES_TO_STORE_PATH_HASHES_FUNCTION_SQL: &str = r#"
CREATE OR REPLACE FUNCTION public.cached_path_references_to_store_path_hashes(text) RETURNS text[]
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    RETURN ARRAY(SELECT split_part(regexp_replace(token, '^/nix/store/', ''), '-', 1)
                 FROM unnest(string_to_array($1, ' ')) token WHERE token <> '');
"#;

const DROP_CACHED_PATH_REFERENCED_STORE_PATH_HASHES_INDEX_SQL: &str = "DROP INDEX CONCURRENTLY IF EXISTS \
    public.\"idx-cached_path-referenced_store_path_hashes\";";

const CREATE_CONCURRENT_GIN_INDEX_ON_CACHED_PATH_REFERENCED_STORE_PATH_HASHES: &str = "CREATE INDEX CONCURRENTLY \
    \"idx-cached_path-referenced_store_path_hashes\" ON public.cached_path \
    USING gin (public.cached_path_references_to_store_path_hashes(\"references\")) WITH (fastupdate = off);";

const MIGRATE_UP_CACHE_DELETION_COUNT_BACKFILL_STALE_MARK_NULL_AND_TRIGGER_SETUP_SQL: &str = r#"
INSERT INTO public.binary_cache_deleted_path_signature_count (cache, deleted_cached_path_signature_count)
SELECT cache.id, 0 FROM public.cache
ON CONFLICT (cache) DO NOTHING;
CREATE FUNCTION public.insert_zero_deleted_cached_path_signature_count_for_new_cache() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO public.binary_cache_deleted_path_signature_count (cache, deleted_cached_path_signature_count)
    SELECT inserted_cache.id, 0 FROM inserted_cache
    ON CONFLICT (cache) DO NOTHING;
    RETURN NULL;
END;
$$;
CREATE TRIGGER after_insert_cache_create_deletion_count_trigger
    AFTER INSERT ON public.cache
    REFERENCING NEW TABLE AS inserted_cache
    FOR EACH STATEMENT EXECUTE FUNCTION public.insert_zero_deleted_cached_path_signature_count_for_new_cache();
UPDATE public.cached_path_signature stale
SET reference_closure_established_at_cache_deletion_count = NULL
FROM public.binary_cache_deleted_path_signature_count current_count
WHERE current_count.cache = stale.cache
  AND stale.reference_closure_established_at_cache_deletion_count <> current_count.deleted_cached_path_signature_count;
CREATE FUNCTION public.clear_reference_closure_mark_and_increment_cache_deletion_count() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO public.binary_cache_deleted_path_signature_count AS counted (cache, deleted_cached_path_signature_count)
    SELECT deleted.cache, 1
    FROM (SELECT DISTINCT deleted_cached_path_signature.cache FROM deleted_cached_path_signature) deleted
    WHERE EXISTS (SELECT 1 FROM public.cache WHERE cache.id = deleted.cache)
    ORDER BY deleted.cache
    ON CONFLICT (cache) DO UPDATE
        SET deleted_cached_path_signature_count = counted.deleted_cached_path_signature_count + 1;
    UPDATE public.cached_path_signature cleared
    SET reference_closure_established_at_cache_deletion_count = NULL
    WHERE cleared.id = ANY(ARRAY(
        WITH RECURSIVE store_path_hash_with_deleted_path_in_reference_closure(cache, hash, id) AS (
            SELECT DISTINCT deleted.cache, deleted.store_path_hash, NULL::uuid
            FROM deleted_cached_path_signature deleted
            WHERE EXISTS (SELECT 1 FROM public.cache WHERE cache.id = deleted.cache)
            UNION
            SELECT referrer_signature.cache, referrer_signature.store_path_hash, referrer_signature.id
            FROM store_path_hash_with_deleted_path_in_reference_closure below
            JOIN public.cached_path referrer
              ON public.cached_path_references_to_store_path_hashes(referrer."references") @> ARRAY[below.hash]
            JOIN public.cached_path_signature referrer_signature
              ON referrer_signature.cache = below.cache AND referrer_signature.store_path_hash = referrer.hash
        )
        SELECT above_deleted.id FROM store_path_hash_with_deleted_path_in_reference_closure above_deleted
        WHERE above_deleted.id IS NOT NULL))
      AND cleared.reference_closure_established_at_cache_deletion_count IS NOT NULL;
    RETURN NULL;
END;
$$;
DROP TRIGGER after_delete_cached_path_signature_trigger ON public.cached_path_signature;
DROP FUNCTION public.increment_cache_deletion_count_after_cached_path_signature_delete();
CREATE TRIGGER after_delete_cached_path_signature_trigger
    AFTER DELETE ON public.cached_path_signature
    REFERENCING OLD TABLE AS deleted_cached_path_signature
    FOR EACH STATEMENT EXECUTE FUNCTION public.clear_reference_closure_mark_and_increment_cache_deletion_count();
"#;

const DOWN_SQL_RESTORE_OLD_DELETE_TRIGGER_FUNCTION_AND_TRIGGER_AND_DROP_CACHE_INSERT_TRIGGER_AND_FUNCTION: &str = r#"
DROP TRIGGER after_delete_cached_path_signature_trigger ON public.cached_path_signature;
DROP FUNCTION public.clear_reference_closure_mark_and_increment_cache_deletion_count();
CREATE FUNCTION public.increment_cache_deletion_count_after_cached_path_signature_delete() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO public.binary_cache_deleted_path_signature_count AS counted (cache, deleted_cached_path_signature_count)
    SELECT deleted.cache, 1
    FROM (SELECT DISTINCT deleted_cached_path_signature.cache FROM deleted_cached_path_signature) deleted
    WHERE EXISTS (SELECT 1 FROM public.cache WHERE cache.id = deleted.cache)
    ORDER BY deleted.cache
    ON CONFLICT (cache) DO UPDATE
        SET deleted_cached_path_signature_count = counted.deleted_cached_path_signature_count + 1;
    RETURN NULL;
END;
$$;
CREATE TRIGGER after_delete_cached_path_signature_trigger
    AFTER DELETE ON public.cached_path_signature
    REFERENCING OLD TABLE AS deleted_cached_path_signature
    FOR EACH STATEMENT EXECUTE FUNCTION public.increment_cache_deletion_count_after_cached_path_signature_delete();
DROP TRIGGER after_insert_cache_create_deletion_count_trigger ON public.cache;
DROP FUNCTION public.insert_zero_deleted_cached_path_signature_count_for_new_cache();
"#;

const DROP_FUNCTION_SQL_FOR_REFERENCES_TEXT_TO_STORE_PATH_HASHES_ON_MIGRATE_DOWN: &str =
    "DROP FUNCTION IF EXISTS public.cached_path_references_to_store_path_hashes(text);";

const RESET_LOCK_TIMEOUT_SQL: &str = "RESET lock_timeout;";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(false)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_sql_statements_on_one_connection(
            manager,
            &[
                LOCK_TIMEOUT_SQL,
                CREATE_OR_REPLACE_CACHED_PATH_REFERENCES_TO_STORE_PATH_HASHES_FUNCTION_SQL,
                DROP_CACHED_PATH_REFERENCED_STORE_PATH_HASHES_INDEX_SQL,
                CREATE_CONCURRENT_GIN_INDEX_ON_CACHED_PATH_REFERENCED_STORE_PATH_HASHES,
                MIGRATE_UP_CACHE_DELETION_COUNT_BACKFILL_STALE_MARK_NULL_AND_TRIGGER_SETUP_SQL,
                RESET_LOCK_TIMEOUT_SQL,
            ],
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_sql_statements_on_one_connection(
            manager,
            &[
                LOCK_TIMEOUT_SQL,
                DOWN_SQL_RESTORE_OLD_DELETE_TRIGGER_FUNCTION_AND_TRIGGER_AND_DROP_CACHE_INSERT_TRIGGER_AND_FUNCTION,
                DROP_CACHED_PATH_REFERENCED_STORE_PATH_HASHES_INDEX_SQL,
                DROP_FUNCTION_SQL_FOR_REFERENCES_TEXT_TO_STORE_PATH_HASHES_ON_MIGRATE_DOWN,
                RESET_LOCK_TIMEOUT_SQL,
            ],
        )
        .await
    }
}
