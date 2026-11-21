/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Workers receive persisted NAR URLs only while the upstream cache row is present. The scheduler
// can't count those URLs as available once that row is gone.
const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
UPDATE public.derivation_output SET external_url = NULL
WHERE external_url IS NOT NULL AND verified_by_upstream IS NULL;
CREATE FUNCTION public.clear_persisted_nar_urls_of_deleted_upstream_cache() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN UPDATE derivation_output SET external_url = NULL, verified_by_upstream = NULL WHERE verified_by_upstream = OLD.id; RETURN OLD; END $$;
CREATE TRIGGER clear_persisted_nar_urls_before_upstream_cache_delete BEFORE DELETE ON public.cache_upstream FOR EACH ROW EXECUTE FUNCTION public.clear_persisted_nar_urls_of_deleted_upstream_cache();
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
DROP TRIGGER clear_persisted_nar_urls_before_upstream_cache_delete ON public.cache_upstream;
DROP FUNCTION public.clear_persisted_nar_urls_of_deleted_upstream_cache();
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
