/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Servers that main built before this change continue to read and write `api_key` during a rolling
// upgrade. Two functions live in one database trigger. Those functions copy that key into
// `encrypted_credential` with the Bearer scheme and copy Bearer credentials back to `api_key`. Both
// server versions still reach a private upstream cache. `api_key`, the trigger and its
// functions go away in the next release.
const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
ALTER TABLE public.cache_upstream
    ADD COLUMN encrypted_credential text,
    ADD COLUMN credential_scheme integer;
UPDATE public.cache_upstream
SET encrypted_credential = api_key, credential_scheme = 0
WHERE api_key IS NOT NULL;

CREATE FUNCTION public.keep_cache_upstream_api_key_and_encrypted_credential_in_sync()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.encrypted_credential IS NULL AND NEW.api_key IS NOT NULL THEN
            NEW.encrypted_credential := NEW.api_key;
            NEW.credential_scheme := 0;
        END IF;
    ELSIF NEW.api_key IS DISTINCT FROM OLD.api_key
        AND NEW.encrypted_credential IS NOT DISTINCT FROM OLD.encrypted_credential THEN
        NEW.encrypted_credential := NEW.api_key;
        NEW.credential_scheme := CASE WHEN NEW.api_key IS NULL THEN NULL ELSE 0 END;
    END IF;
    NEW.api_key := CASE WHEN NEW.credential_scheme = 0 THEN NEW.encrypted_credential END;
    RETURN NEW;
END;
$$;

CREATE TRIGGER sync_cache_upstream_api_key_and_encrypted_credential
BEFORE INSERT OR UPDATE ON public.cache_upstream
FOR EACH ROW
EXECUTE FUNCTION public.keep_cache_upstream_api_key_and_encrypted_credential_in_sync();
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
DROP TRIGGER sync_cache_upstream_api_key_and_encrypted_credential ON public.cache_upstream;
DROP FUNCTION public.keep_cache_upstream_api_key_and_encrypted_credential_in_sync();
UPDATE public.cache_upstream
SET api_key = encrypted_credential
WHERE credential_scheme = 0 AND api_key IS DISTINCT FROM encrypted_credential;
ALTER TABLE public.cache_upstream
    DROP COLUMN credential_scheme,
    DROP COLUMN encrypted_credential;
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
