/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// SCIM accounts without externalId are indistinguishable from accounts in the state file. These
// accounts get the declared status from the backfill. When the state file does not declare them,
// startup routines delete them or clear the managed flag, as before. Later syncs from the identity
// provider then provision them again as SCIM accounts.
const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
ALTER TABLE public."user" ADD COLUMN account_origin SMALLINT NOT NULL DEFAULT 0;
UPDATE public."user" SET account_origin = CASE
    WHEN managed AND scim_external_id IS NOT NULL AND password IS NOT NULL THEN 1
    WHEN scim_external_id IS NOT NULL THEN 2
    WHEN managed THEN 1
    WHEN password IS NULL AND EXISTS (
        SELECT 1 FROM public.user_identity WHERE user_identity."user" = "user".id
    ) THEN 3
    ELSE 0
END;
UPDATE public."user" SET scim_external_id = NULL WHERE account_origin = 1;

-- Servers that were built from main before this change insert SCIM accounts
-- without `account_origin` during a rolling upgrade. Those accounts would become
-- public registrations (0) without this trigger. This trigger can mark them as
-- SCIM-provisioned. The identity provider can still find and deactivate them.
-- The next release can drop this trigger and its function.
CREATE FUNCTION public.set_scim_account_origin_on_user_insert_from_old_server()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.account_origin = 0 AND NEW.scim_external_id IS NOT NULL THEN
        NEW.account_origin := 2;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER set_account_origin_for_scim_user_before_insert
BEFORE INSERT ON public."user"
FOR EACH ROW
EXECUTE FUNCTION public.set_scim_account_origin_on_user_insert_from_old_server();
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
DROP TRIGGER set_account_origin_for_scim_user_before_insert ON public."user";
DROP FUNCTION public.set_scim_account_origin_on_user_insert_from_old_server();
ALTER TABLE public."user" DROP COLUMN account_origin;
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
