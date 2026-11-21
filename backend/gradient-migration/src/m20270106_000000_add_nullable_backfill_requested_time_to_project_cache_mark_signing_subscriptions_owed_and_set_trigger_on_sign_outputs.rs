/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
ALTER TABLE public.project_cache ADD COLUMN unsigned_signature_backfill_requested_at timestamp without time zone NULL;
-- An upgrade can mark as owed the subscriptions that already sign outputs. An earlier migration did not write those placeholders. The old request path skipped subscriptions that came from approved requests and from the state file.
UPDATE public.project_cache SET unsigned_signature_backfill_requested_at = (now() AT TIME ZONE 'UTC') WHERE sign_outputs;
CREATE FUNCTION public.set_unsigned_signature_backfill_requested_at_from_sign_outputs() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NOT NEW.sign_outputs THEN
        NEW.unsigned_signature_backfill_requested_at := NULL;
    ELSIF TG_OP = 'INSERT' OR NOT OLD.sign_outputs THEN
        NEW.unsigned_signature_backfill_requested_at := (now() AT TIME ZONE 'UTC');
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER set_sign_backfill_requested_at_on_sign_outputs_change
    BEFORE INSERT OR UPDATE ON public.project_cache
    FOR EACH ROW EXECUTE FUNCTION public.set_unsigned_signature_backfill_requested_at_from_sign_outputs();
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
DROP TRIGGER set_sign_backfill_requested_at_on_sign_outputs_change ON public.project_cache;
DROP FUNCTION public.set_unsigned_signature_backfill_requested_at_from_sign_outputs();
ALTER TABLE public.project_cache DROP COLUMN unsigned_signature_backfill_requested_at;
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
