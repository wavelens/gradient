/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
UPDATE public.team_user tu SET source = 3
FROM public."user" u, public.team t
WHERE tu."user" = u.id
    AND tu.team = t.id
    AND tu.source = 2
    AND (u.account_origin <> 2 OR t.scim_group IS NULL);

-- Servers built from main before this change still write source 2 for sign-in
-- group sync during a rolling upgrade. The trigger and the rules of the UPDATE
-- send those rows to source 3, so sign-in group sync can still delete them. The
-- next release can drop the trigger and its function.
CREATE FUNCTION public.set_team_user_source_from_scim_group_to_sign_in_sync()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.source = 2 AND (
        NOT EXISTS (
            SELECT 1 FROM public."user" u WHERE u.id = NEW."user" AND u.account_origin = 2
        )
        OR NOT EXISTS (
            SELECT 1 FROM public.team t WHERE t.id = NEW.team AND t.scim_group IS NOT NULL
        )
    ) THEN
        NEW.source := 3;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER team_user_before_insert_or_update_remap_source_2_to_3
BEFORE INSERT OR UPDATE OF source ON public.team_user
FOR EACH ROW
EXECUTE FUNCTION public.set_team_user_source_from_scim_group_to_sign_in_sync();
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
DROP TRIGGER team_user_before_insert_or_update_remap_source_2_to_3 ON public.team_user;
DROP FUNCTION public.set_team_user_source_from_scim_group_to_sign_in_sync();
UPDATE public.team_user SET source = 2 WHERE source = 3;
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
