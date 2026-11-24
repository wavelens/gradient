/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
CREATE TABLE public.binary_cache_deleted_path_signature_count (
    cache uuid PRIMARY KEY REFERENCES public.cache(id) ON DELETE CASCADE,
    deleted_cached_path_signature_count bigint NOT NULL
);
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
ALTER TABLE public.cached_path_signature
    ADD COLUMN reference_closure_established_at_cache_deletion_count bigint NULL,
    DROP COLUMN references_are_claimed;
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
ALTER TABLE public.cached_path_signature
    ADD COLUMN references_are_claimed boolean NOT NULL DEFAULT false,
    DROP COLUMN reference_closure_established_at_cache_deletion_count;
DROP TRIGGER after_delete_cached_path_signature_trigger ON public.cached_path_signature;
DROP FUNCTION public.increment_cache_deletion_count_after_cached_path_signature_delete();
DROP TABLE public.binary_cache_deleted_path_signature_count;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
