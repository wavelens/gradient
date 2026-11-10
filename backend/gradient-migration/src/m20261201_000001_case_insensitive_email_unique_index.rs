/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Case differences can make one account's email address equal another account's email address. Such accounts conflict with the unique index on lower(email). The migration kept the email address on the state-managed account first. The migration then kept the email address on verified accounts. The migration kept the email address on the oldest account last. All other accounts lost the email address. They lost the verification state. The owners can re-enter the email address in settings.
const CLEAR_CASE_DUPLICATES: &str = r#"
DO $$
DECLARE
    duplicate RECORD;
BEGIN
    FOR duplicate IN
        SELECT id, email FROM (
            SELECT id, email, row_number() OVER (
                PARTITION BY lower(email) ORDER BY managed DESC, email_verified DESC, created_at, id
            ) AS position
            FROM public."user"
            WHERE email <> ''
        ) ranked
        WHERE position > 1
    LOOP
        UPDATE public."user"
        SET email = '', email_verified = false,
            email_verification_token = NULL, email_verification_token_expires = NULL
        WHERE id = duplicate.id;
        RAISE NOTICE 'cleared email % of user % because another account keeps the same email apart from case',
            duplicate.email, duplicate.id;
    END LOOP;
END $$;
"#;

const UP: &str =
    "CREATE UNIQUE INDEX user_email_key ON public.\"user\" (lower(email)) WHERE email <> '';";

const DOWN: &str = "DROP INDEX public.user_email_key;";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(CLEAR_CASE_DUPLICATES).await?;
        db.execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
